//! `saved_views` rows (spec 04): saved list views and SQL queries (D-H-6).

use llmobs_core::ids::ProjectId;
use serde_json::{Map, Value};
use sqlx::{AssertSqlSafe, SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::MetaError;
use crate::api::saved_views::{SavedView, SavedViewKind};

/// Most views one user may own in one project (D-H-6).
pub(crate) const MAX_VIEWS_PER_OWNER: i64 = 1000;

/// Unsharing a shared view whose owner was deleted would hide it from everyone (D-H-6).
pub(crate) const UNSHARE_ORPHAN: &str = "the view has no owner; delete it instead";

// The SQL text of this module is built only from the constants here and the fixed
// clauses of `visibility_clause`; every caller value is bound, never spliced in.
/// Columns selected by every read, in the order [`SavedViewRecord`] declares them.
const COLUMNS: &str =
    "id, kind, name, state, owner_user_id, shared, is_default, created_at, updated_at";

/// Who is reading. A user sees their own views and every shared view; a key sees shared views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Viewer {
    /// A session or PAT user, by `users.id`.
    User(Uuid),
    /// A project API key.
    Key,
}

/// One stored view.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SavedViewRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Which page the view opens.
    pub(crate) kind: SavedViewKind,
    /// Display name.
    pub(crate) name: String,
    /// Opaque UI state.
    pub(crate) state: Map<String, Value>,
    /// Owner. Absent once the owner was deleted.
    pub(crate) owner_user_id: Option<Uuid>,
    /// Visible to every key and user of the project.
    pub(crate) shared: bool,
    /// The default of its scope.
    pub(crate) is_default: bool,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change, nanoseconds.
    pub(crate) updated_at: i64,
}

/// One page of [`list_visible`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SavedViewPage {
    /// Rows in id order.
    pub(crate) rows: Vec<SavedViewRow>,
    /// The last row's id when more rows follow, else `None`.
    pub(crate) next_cursor: Option<String>,
}

/// Columns of a new view. `name` is already trimmed and checked; `state` is already checked.
#[derive(Debug, Clone)]
pub(crate) struct NewSavedView {
    /// Project the view belongs to.
    pub(crate) project_id: ProjectId,
    /// Which page the view opens.
    pub(crate) kind: SavedViewKind,
    /// Display name.
    pub(crate) name: String,
    /// Opaque UI state.
    pub(crate) state: Map<String, Value>,
    /// Owner.
    pub(crate) owner_user_id: Uuid,
    /// Visible to the whole project.
    pub(crate) shared: bool,
    /// The default of its scope.
    pub(crate) is_default: bool,
}

/// What [`update_view`] changes. `None` leaves the column alone.
#[derive(Debug, Clone, Default)]
pub(crate) struct SavedViewChanges {
    /// New display name.
    pub(crate) name: Option<String>,
    /// New UI state.
    pub(crate) state: Option<Map<String, Value>>,
    /// New sharing flag.
    pub(crate) shared: Option<bool>,
    /// New default flag.
    pub(crate) is_default: Option<bool>,
}

/// A `saved_views` row as SQLite returns it.
#[derive(Debug, sqlx::FromRow)]
struct SavedViewRecord {
    id: String,
    kind: String,
    name: String,
    state: String,
    owner_user_id: Option<String>,
    shared: i64,
    is_default: i64,
    created_at: i64,
    updated_at: i64,
}

/// The `kind` column text.
pub(crate) fn kind_str(kind: SavedViewKind) -> &'static str {
    match kind {
        SavedViewKind::Traces => "traces",
        SavedViewKind::Spans => "spans",
        SavedViewKind::Sessions => "sessions",
        SavedViewKind::Users => "users",
        SavedViewKind::Sql => "sql",
    }
}

fn parse_kind(text: &str) -> Option<SavedViewKind> {
    match text {
        "traces" => Some(SavedViewKind::Traces),
        "spans" => Some(SavedViewKind::Spans),
        "sessions" => Some(SavedViewKind::Sessions),
        "users" => Some(SavedViewKind::Users),
        "sql" => Some(SavedViewKind::Sql),
        _ => None,
    }
}

/// Whether `viewer` may see `row`: a user sees rows they own and shared rows; a key sees
/// shared rows. A private row with no owner is visible to nobody.
pub(crate) fn visible_to(row: &SavedViewRow, viewer: Viewer) -> bool {
    row.shared || matches!(viewer, Viewer::User(id) if row.owner_user_id == Some(id))
}

fn row_from(record: SavedViewRecord) -> Result<SavedViewRow, MetaError> {
    let corrupt = |column: &'static str| MetaError::Corrupt {
        table: "saved_views",
        column,
    };
    let id = Uuid::parse_str(&record.id).map_err(|_| corrupt("id"))?;
    let kind = parse_kind(&record.kind).ok_or_else(|| corrupt("kind"))?;
    let state =
        serde_json::from_str::<Map<String, Value>>(&record.state).map_err(|_| corrupt("state"))?;
    let owner_user_id = record
        .owner_user_id
        .as_deref()
        .map(|text| Uuid::parse_str(text).map_err(|_| corrupt("owner_user_id")))
        .transpose()?;
    Ok(SavedViewRow {
        id,
        kind,
        name: record.name,
        state,
        owner_user_id,
        shared: record.shared != 0,
        is_default: record.is_default != 0,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

/// The visibility condition for `viewer`. Its bound values follow the other binds in order.
fn visibility_clause(viewer: Viewer) -> &'static str {
    match viewer {
        Viewer::User(_) => " AND (shared = 1 OR owner_user_id = ?)",
        Viewer::Key => " AND shared = 1",
    }
}

fn state_text(state: &Map<String, Value>) -> Result<String, MetaError> {
    serde_json::to_string(state).map_err(|_| MetaError::Corrupt {
        table: "saved_views",
        column: "state",
    })
}

/// Views of `project` that `viewer` may see, optionally of one `kind`, ordered by `id`
/// ascending (creation order). `cursor` is the last id of the previous page: rows with a
/// greater `id` follow. `limit` is 1–1000; the route checks it.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_visible(
    pool: &SqlitePool,
    project: ProjectId,
    viewer: Viewer,
    kind: Option<SavedViewKind>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<SavedViewPage, MetaError> {
    let fetch = limit
        .checked_add(1)
        .and_then(|n| i64::try_from(n).ok())
        .ok_or(MetaError::Conflict("page size is too large"))?;
    let mut sql = format!("SELECT {COLUMNS} FROM saved_views WHERE project_id = ?");
    if kind.is_some() {
        sql.push_str(" AND kind = ?");
    }
    sql.push_str(visibility_clause(viewer));
    if cursor.is_some() {
        sql.push_str(" AND id > ?");
    }
    sql.push_str(" ORDER BY id LIMIT ?");

    let mut query =
        sqlx::query_as::<_, SavedViewRecord>(AssertSqlSafe(sql)).bind(project.to_string());
    if let Some(kind) = kind {
        query = query.bind(kind_str(kind));
    }
    if let Viewer::User(id) = viewer {
        query = query.bind(id.to_string());
    }
    if let Some(cursor) = cursor {
        query = query.bind(cursor);
    }
    let records = query.bind(fetch).fetch_all(pool).await?;

    let mut rows = records
        .into_iter()
        .map(row_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| row.id.to_string())
    } else {
        None
    };
    Ok(SavedViewPage { rows, next_cursor })
}

/// The view `id` of `project` when `viewer` may see it.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn get_visible(
    pool: &SqlitePool,
    project: ProjectId,
    viewer: Viewer,
    id: Uuid,
) -> Result<Option<SavedViewRow>, MetaError> {
    let mut sql = format!("SELECT {COLUMNS} FROM saved_views WHERE project_id = ? AND id = ?");
    sql.push_str(visibility_clause(viewer));
    let mut query = sqlx::query_as::<_, SavedViewRecord>(AssertSqlSafe(sql))
        .bind(project.to_string())
        .bind(id.to_string());
    if let Viewer::User(user) = viewer {
        query = query.bind(user.to_string());
    }
    let record = query.fetch_optional(pool).await?;
    record.map(row_from).transpose()
}

/// Every visible view of `kind` named exactly `name`, the viewer's own first, then by id.
///
/// Names are not unique (D-H-6): several visible views may share a name, which is why this
/// returns a list.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn find_visible_by_name(
    pool: &SqlitePool,
    project: ProjectId,
    viewer: Viewer,
    kind: SavedViewKind,
    name: &str,
) -> Result<Vec<SavedViewRow>, MetaError> {
    let mut sql =
        format!("SELECT {COLUMNS} FROM saved_views WHERE project_id = ? AND kind = ? AND name = ?");
    sql.push_str(visibility_clause(viewer));
    sql.push_str(" ORDER BY id");
    let mut query = sqlx::query_as::<_, SavedViewRecord>(AssertSqlSafe(sql))
        .bind(project.to_string())
        .bind(kind_str(kind))
        .bind(name);
    if let Viewer::User(user) = viewer {
        query = query.bind(user.to_string());
    }
    let records = query.fetch_all(pool).await?;
    let mut rows = records
        .into_iter()
        .map(row_from)
        .collect::<Result<Vec<_>, _>>()?;
    // The sort is stable, so each group keeps the id order of the query.
    rows.sort_by_key(|row| {
        let own = matches!(viewer, Viewer::User(me) if row.owner_user_id == Some(me));
        !own
    });
    Ok(rows)
}

/// The view `id` of `project`, whoever owns it. Callers check visibility and permissions.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn view_by_id(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<SavedViewRow>, MetaError> {
    let sql = format!("SELECT {COLUMNS} FROM saved_views WHERE project_id = ? AND id = ?");
    let record: Option<SavedViewRecord> = sqlx::query_as(AssertSqlSafe(sql))
        .bind(project.to_string())
        .bind(id.to_string())
        .fetch_optional(&mut *conn)
        .await?;
    record.map(row_from).transpose()
}

/// Inserts a view. When the owner already owns [`MAX_VIEWS_PER_OWNER`] views in the project,
/// returns `MetaError::Conflict("too many saved views")`. When `is_default` is set, clears
/// the other defaults of the same scope first (see [`clear_other_defaults`]).
///
/// # Errors
///
/// Returns [`MetaError`] when the count or the insert fails.
pub(crate) async fn insert_view(
    conn: &mut SqliteConnection,
    view: NewSavedView,
    now: i64,
) -> Result<SavedViewRow, MetaError> {
    let project = view.project_id;
    let owner = view.owner_user_id;
    let owned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM saved_views WHERE project_id = ? AND owner_user_id = ?",
    )
    .bind(project.to_string())
    .bind(owner.to_string())
    .fetch_one(&mut *conn)
    .await?;
    if owned >= MAX_VIEWS_PER_OWNER {
        return Err(MetaError::Conflict("too many saved views"));
    }

    let state = state_text(&view.state)?;
    let row = SavedViewRow {
        id: Uuid::now_v7(),
        kind: view.kind,
        name: view.name,
        state: view.state,
        owner_user_id: Some(owner),
        shared: view.shared,
        is_default: view.is_default,
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        "INSERT INTO saved_views \
         (id, project_id, kind, name, state, owner_user_id, shared, is_default, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.id.to_string())
    .bind(project.to_string())
    .bind(kind_str(row.kind))
    .bind(row.name.as_str())
    .bind(state)
    .bind(owner.to_string())
    .bind(i64::from(row.shared))
    .bind(i64::from(row.is_default))
    .bind(row.created_at)
    .bind(row.updated_at)
    .execute(&mut *conn)
    .await?;
    if row.is_default {
        clear_other_defaults(conn, project, &row).await?;
    }
    Ok(row)
}

/// Applies `changes` to the view `id` of `project` and returns the row after the change, or
/// `None` when there is no such view. `updated_at` becomes `now` when anything changed.
///
/// Unsharing a shared view drops its default, unless the same change sets `is_default`.
/// Unsharing a shared view that has no owner is `MetaError::Conflict(UNSHARE_ORPHAN)`.
///
/// # Errors
///
/// Returns [`MetaError`] when a query fails, a stored value does not decode, or the change
/// is refused.
pub(crate) async fn update_view(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
    changes: SavedViewChanges,
    now: i64,
) -> Result<Option<SavedViewRow>, MetaError> {
    let Some(before) = view_by_id(conn, project, id).await? else {
        return Ok(None);
    };
    if before.shared && before.owner_user_id.is_none() && changes.shared == Some(false) {
        return Err(MetaError::Conflict(UNSHARE_ORPHAN));
    }

    let mut row = before.clone();
    if let Some(name) = changes.name {
        row.name = name;
    }
    if let Some(state) = changes.state {
        row.state = state;
    }
    if let Some(shared) = changes.shared {
        row.shared = shared;
    }
    if let Some(is_default) = changes.is_default {
        row.is_default = is_default;
    }
    if before.shared && !row.shared && changes.is_default.is_none() {
        row.is_default = false;
    }
    if row == before {
        return Ok(Some(row));
    }

    row.updated_at = now;
    let state = state_text(&row.state)?;
    sqlx::query(
        "UPDATE saved_views SET name = ?, state = ?, shared = ?, is_default = ?, updated_at = ? \
         WHERE project_id = ? AND id = ?",
    )
    .bind(row.name.as_str())
    .bind(state)
    .bind(i64::from(row.shared))
    .bind(i64::from(row.is_default))
    .bind(now)
    .bind(project.to_string())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    if row.is_default {
        clear_other_defaults(conn, project, &row).await?;
    }
    Ok(Some(row))
}

/// Deletes the view `id` of `project`. Returns whether a row was deleted.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_view(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query("DELETE FROM saved_views WHERE project_id = ? AND id = ?")
        .bind(project.to_string())
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Clears `is_default` on every other view of the same scope as `row`: the shared views of
/// `row.kind` when `row.shared`, else the owner's private views of `row.kind`.
async fn clear_other_defaults(
    conn: &mut SqliteConnection,
    project: ProjectId,
    row: &SavedViewRow,
) -> Result<(), MetaError> {
    let kind = kind_str(row.kind);
    let id = row.id.to_string();
    if row.shared {
        sqlx::query(
            "UPDATE saved_views SET is_default = 0 \
             WHERE project_id = ? AND kind = ? AND shared = 1 AND id <> ?",
        )
        .bind(project.to_string())
        .bind(kind)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    } else if let Some(owner) = row.owner_user_id {
        sqlx::query(
            "UPDATE saved_views SET is_default = 0 \
             WHERE project_id = ? AND kind = ? AND shared = 0 AND owner_user_id = ? AND id <> ?",
        )
        .bind(project.to_string())
        .bind(kind)
        .bind(owner.to_string())
        .bind(id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

impl From<SavedViewRow> for SavedView {
    fn from(row: SavedViewRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            name: row.name,
            state: row.state,
            owner_user_id: row.owner_user_id,
            shared: row.shared,
            is_default: row.is_default,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::meta::{MetaDb, orgs, projects, users};

    const NOW: i64 = 1_767_225_600_000_000_000;

    /// One org, two projects (`support-bot`, `other-bot`), and three users: `owner`, `other`,
    /// `third`.
    struct World {
        project: ProjectId,
        second: ProjectId,
        owner: Uuid,
        other: Uuid,
        third: Uuid,
    }

    fn new_user(email: &str) -> users::NewUser {
        users::NewUser {
            email: email.to_owned(),
            name: None,
            password_hash: None,
            email_verified_at: None,
            is_instance_admin: false,
        }
    }

    async fn world(db: &MetaDb) -> World {
        db.write(|conn| {
            Box::pin(async move {
                let owner = users::insert_user(conn, new_user("owner@example.com"), NOW)
                    .await?
                    .id;
                let other = users::insert_user(conn, new_user("other@example.com"), NOW)
                    .await?
                    .id;
                let third = users::insert_user(conn, new_user("third@example.com"), NOW)
                    .await?
                    .id;
                let org = orgs::insert_org(conn, "Acme", "acme", NOW).await?;
                let project =
                    projects::insert_project(conn, org.id, "Support", "support-bot", None, NOW)
                        .await?
                        .id;
                let second =
                    projects::insert_project(conn, org.id, "Other", "other-bot", None, NOW)
                        .await?
                        .id;
                Ok(World {
                    project,
                    second,
                    owner,
                    other,
                    third,
                })
            })
        })
        .await
        .unwrap()
    }

    fn state(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    /// A view with state `{"filter": "status = 'error'"}`.
    fn new_view(
        project: ProjectId,
        owner: Uuid,
        kind: SavedViewKind,
        name: &str,
        shared: bool,
        is_default: bool,
    ) -> NewSavedView {
        NewSavedView {
            project_id: project,
            kind,
            name: name.to_owned(),
            state: state(json!({"filter": "status = 'error'"})),
            owner_user_id: owner,
            shared,
            is_default,
        }
    }

    /// `insert_view` in its own `db.write`.
    async fn insert(db: &MetaDb, view: NewSavedView, now: i64) -> SavedViewRow {
        db.write(move |conn| Box::pin(async move { insert_view(conn, view, now).await }))
            .await
            .unwrap()
    }

    /// `update_view` in its own `db.write`.
    async fn update(
        db: &MetaDb,
        project: ProjectId,
        id: Uuid,
        changes: SavedViewChanges,
        now: i64,
    ) -> Result<Option<SavedViewRow>, MetaError> {
        db.write(move |conn| {
            Box::pin(async move { update_view(conn, project, id, changes, now).await })
        })
        .await
    }

    /// The stored row, whoever may see it (`view_by_id` in a `db.write`).
    async fn stored(db: &MetaDb, project: ProjectId, id: Uuid) -> Option<SavedViewRow> {
        db.write(move |conn| Box::pin(async move { view_by_id(conn, project, id).await }))
            .await
            .unwrap()
    }

    fn ids(rows: &[SavedViewRow]) -> Vec<Uuid> {
        rows.iter().map(|row| row.id).collect()
    }

    fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn insert_then_get_round_trips() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let row = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Errors",
                false,
                false,
            ),
            NOW,
        )
        .await;
        assert_eq!(row.name, "Errors");
        assert_eq!(row.kind, SavedViewKind::Traces);
        assert_eq!(row.state, state(json!({"filter": "status = 'error'"})));
        assert_eq!(row.owner_user_id, Some(w.owner));
        assert!(!row.shared);
        assert!(!row.is_default);
        assert_eq!(row.created_at, NOW);
        assert_eq!(row.updated_at, NOW);

        let got = get_visible(db.reader(), w.project, Viewer::User(w.owner), row.id)
            .await
            .unwrap();
        assert_eq!(got, Some(row));
    }

    #[tokio::test]
    async fn visibility_follows_owner_and_shared() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let private = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Private",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let shared = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Shared",
                true,
                false,
            ),
            NOW,
        )
        .await;

        let for_owner = list_visible(
            db.reader(),
            w.project,
            Viewer::User(w.owner),
            None,
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ids(&for_owner.rows), sorted(vec![private.id, shared.id]));
        let for_other = list_visible(
            db.reader(),
            w.project,
            Viewer::User(w.other),
            None,
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ids(&for_other.rows), vec![shared.id]);
        let for_key = list_visible(db.reader(), w.project, Viewer::Key, None, 10, None)
            .await
            .unwrap();
        assert_eq!(ids(&for_key.rows), vec![shared.id]);

        for viewer in [Viewer::User(w.other), Viewer::Key] {
            let got = get_visible(db.reader(), w.project, viewer, private.id)
                .await
                .unwrap();
            assert_eq!(got, None);
        }
    }

    #[tokio::test]
    async fn list_without_kind_returns_every_kind() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        for kind in [
            SavedViewKind::Traces,
            SavedViewKind::Spans,
            SavedViewKind::Sessions,
            SavedViewKind::Users,
            SavedViewKind::Sql,
        ] {
            insert(
                &db,
                new_view(w.project, w.owner, kind, "view", false, false),
                NOW,
            )
            .await;
        }
        let page = list_visible(
            db.reader(),
            w.project,
            Viewer::User(w.owner),
            None,
            50,
            None,
        )
        .await
        .unwrap();
        assert_eq!(page.rows.len(), 5);
        let mut kinds: Vec<&str> = page.rows.iter().map(|row| kind_str(row.kind)).collect();
        kinds.sort();
        assert_eq!(kinds, ["sessions", "spans", "sql", "traces", "users"]);
    }

    #[tokio::test]
    async fn kind_filter_and_pages() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let mut sql_ids = Vec::new();
        for index in 0..3 {
            let row = insert(
                &db,
                new_view(
                    w.project,
                    w.owner,
                    SavedViewKind::Sql,
                    &format!("query {index}"),
                    false,
                    false,
                ),
                NOW,
            )
            .await;
            sql_ids.push(row.id);
        }
        insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "traces",
                false,
                false,
            ),
            NOW,
        )
        .await;
        sql_ids.sort();

        let first = list_visible(
            db.reader(),
            w.project,
            Viewer::User(w.owner),
            Some(SavedViewKind::Sql),
            2,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ids(&first.rows), sql_ids[..2].to_vec());
        let cursor = sql_ids[1].to_string();
        assert_eq!(first.next_cursor, Some(cursor.clone()));

        let second = list_visible(
            db.reader(),
            w.project,
            Viewer::User(w.owner),
            Some(SavedViewKind::Sql),
            2,
            Some(cursor.as_str()),
        )
        .await
        .unwrap();
        assert_eq!(ids(&second.rows), vec![sql_ids[2]]);
        assert_eq!(second.next_cursor, None);
    }

    #[tokio::test]
    async fn a_full_last_page_has_no_cursor() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let a = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "a", false, false),
            NOW,
        )
        .await;
        let b = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "b", false, false),
            NOW,
        )
        .await;
        let page = list_visible(db.reader(), w.project, Viewer::User(w.owner), None, 2, None)
            .await
            .unwrap();
        assert_eq!(ids(&page.rows), sorted(vec![a.id, b.id]));
        assert_eq!(page.next_cursor, None);
    }

    #[tokio::test]
    async fn projects_are_isolated() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let v = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Here",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let there = insert(
            &db,
            new_view(
                w.second,
                w.owner,
                SavedViewKind::Traces,
                "There",
                false,
                false,
            ),
            NOW,
        )
        .await;

        let page = list_visible(db.reader(), w.second, Viewer::User(w.owner), None, 10, None)
            .await
            .unwrap();
        assert_eq!(ids(&page.rows), vec![there.id]);
        let got = get_visible(db.reader(), w.second, Viewer::User(w.owner), v.id)
            .await
            .unwrap();
        assert_eq!(got, None);
        assert!(stored(&db, w.second, v.id).await.is_none());
        let updated = update(
            &db,
            w.second,
            v.id,
            SavedViewChanges {
                name: Some("x".to_owned()),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap();
        assert!(updated.is_none());
        let deleted = db
            .write(move |conn| Box::pin(async move { delete_view(conn, w.second, v.id).await }))
            .await
            .unwrap();
        assert!(!deleted);
        let still = stored(&db, w.project, v.id).await.unwrap();
        assert_eq!(still.name, "Here");

        let s1 = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "S1", true, true),
            NOW,
        )
        .await;
        let s2 = insert(
            &db,
            new_view(w.second, w.owner, SavedViewKind::Traces, "S2", true, true),
            NOW,
        )
        .await;
        assert!(stored(&db, w.project, s1.id).await.unwrap().is_default);
        assert!(stored(&db, w.second, s2.id).await.unwrap().is_default);
    }

    #[tokio::test]
    async fn deleting_the_project_deletes_its_views() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "kept",
                false,
                false,
            ),
            NOW,
        )
        .await;
        for name in ["gone a", "gone b"] {
            insert(
                &db,
                new_view(w.second, w.owner, SavedViewKind::Traces, name, false, false),
                NOW,
            )
            .await;
        }
        let second = w.second;
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("DELETE FROM projects WHERE id = ?")
                    .bind(second.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM saved_views WHERE project_id = ?")
            .bind(w.second.to_string())
            .fetch_one(db.reader())
            .await
            .unwrap();
        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM saved_views WHERE project_id = ?")
            .bind(w.project.to_string())
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(gone, 0);
        assert_eq!(kept, 1);
    }

    #[tokio::test]
    async fn deleting_the_owner_hides_private_views() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let private = insert(
            &db,
            new_view(
                w.project,
                w.third,
                SavedViewKind::Traces,
                "Private",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let shared = insert(
            &db,
            new_view(
                w.project,
                w.third,
                SavedViewKind::Traces,
                "Shared",
                true,
                false,
            ),
            NOW,
        )
        .await;
        let third = w.third;
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("DELETE FROM users WHERE id = ?")
                    .bind(third.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let page = list_visible(
            db.reader(),
            w.project,
            Viewer::User(w.owner),
            None,
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ids(&page.rows), vec![shared.id]);
        assert_eq!(page.rows[0].owner_user_id, None);
        let orphan = stored(&db, w.project, private.id).await.unwrap();
        assert_eq!(orphan.owner_user_id, None);
        assert!(!orphan.shared);
    }

    #[tokio::test]
    async fn private_default_clears_only_the_owners_other_default() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let p1 = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P1", false, true),
            NOW,
        )
        .await;
        let p2 = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P2", false, true),
            NOW,
        )
        .await;
        let q = insert(
            &db,
            new_view(w.project, w.other, SavedViewKind::Traces, "Q", false, true),
            NOW,
        )
        .await;
        assert!(!stored(&db, w.project, p1.id).await.unwrap().is_default);
        assert!(stored(&db, w.project, p2.id).await.unwrap().is_default);
        assert!(stored(&db, w.project, q.id).await.unwrap().is_default);
    }

    #[tokio::test]
    async fn shared_default_clears_other_shared_defaults() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let s1 = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "S1", true, true),
            NOW,
        )
        .await;
        let s2 = insert(
            &db,
            new_view(w.project, w.other, SavedViewKind::Traces, "S2", true, true),
            NOW,
        )
        .await;
        let p = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P", false, true),
            NOW,
        )
        .await;
        assert!(!stored(&db, w.project, s1.id).await.unwrap().is_default);
        assert!(stored(&db, w.project, s2.id).await.unwrap().is_default);
        assert!(stored(&db, w.project, p.id).await.unwrap().is_default);

        let changed = update(
            &db,
            w.project,
            s1.id,
            SavedViewChanges {
                is_default: Some(true),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(changed.is_default);
        assert!(!stored(&db, w.project, s2.id).await.unwrap().is_default);
        assert!(stored(&db, w.project, p.id).await.unwrap().is_default);
    }

    #[tokio::test]
    async fn defaults_are_per_kind() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let t = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "T", false, true),
            NOW,
        )
        .await;
        let q = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Sql, "Q", false, true),
            NOW,
        )
        .await;
        assert!(stored(&db, w.project, t.id).await.unwrap().is_default);
        assert!(stored(&db, w.project, q.id).await.unwrap().is_default);
    }

    #[tokio::test]
    async fn unsharing_clears_the_default() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let s = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "S", true, true),
            NOW,
        )
        .await;
        let row = update(
            &db,
            w.project,
            s.id,
            SavedViewChanges {
                shared: Some(false),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!row.shared);
        assert!(!row.is_default);
        assert_eq!(row.updated_at, NOW + 1);
    }

    #[tokio::test]
    async fn unsharing_with_a_default_keeps_it_in_the_private_scope() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let p = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P", false, true),
            NOW,
        )
        .await;
        let s = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "S", true, true),
            NOW,
        )
        .await;
        let row = update(
            &db,
            w.project,
            s.id,
            SavedViewChanges {
                shared: Some(false),
                is_default: Some(true),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!row.shared);
        assert!(row.is_default);
        assert!(!stored(&db, w.project, p.id).await.unwrap().is_default);
    }

    #[tokio::test]
    async fn unsharing_a_private_view_changes_nothing() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let p = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P", false, true),
            NOW,
        )
        .await;
        let row = update(
            &db,
            w.project,
            p.id,
            SavedViewChanges {
                shared: Some(false),
                ..Default::default()
            },
            NOW + 3,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(row.is_default);
        assert_eq!(row.updated_at, NOW);
    }

    #[tokio::test]
    async fn sharing_a_private_default_takes_over_the_project_default() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let s1 = insert(
            &db,
            new_view(w.project, w.other, SavedViewKind::Traces, "S1", true, true),
            NOW,
        )
        .await;
        let p = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P", false, true),
            NOW,
        )
        .await;
        let row = update(
            &db,
            w.project,
            p.id,
            SavedViewChanges {
                shared: Some(true),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(row.shared);
        assert!(row.is_default);
        assert!(!stored(&db, w.project, s1.id).await.unwrap().is_default);
    }

    #[tokio::test]
    async fn clearing_a_default_touches_nothing_else() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let p1 = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "P1", false, true),
            NOW,
        )
        .await;
        let p2 = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "P2",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let row = update(
            &db,
            w.project,
            p1.id,
            SavedViewChanges {
                is_default: Some(false),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!row.is_default);
        let other = stored(&db, w.project, p2.id).await.unwrap();
        assert!(!other.is_default);
        assert_eq!(other.updated_at, NOW);
    }

    #[tokio::test]
    async fn an_orphan_private_default_is_left_alone() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let p = insert(
            &db,
            new_view(w.project, w.third, SavedViewKind::Traces, "P", false, true),
            NOW,
        )
        .await;
        let third = w.third;
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("DELETE FROM users WHERE id = ?")
                    .bind(third.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "R", false, true),
            NOW,
        )
        .await;
        let orphan = stored(&db, w.project, p.id).await.unwrap();
        assert_eq!(orphan.owner_user_id, None);
        assert!(orphan.is_default);
    }

    #[tokio::test]
    async fn name_and_state_changes_bump_updated_at() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let v = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Errors",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let renamed = update(
            &db,
            w.project,
            v.id,
            SavedViewChanges {
                name: Some("Renamed".to_owned()),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(renamed.name, "Renamed");
        assert_eq!(renamed.updated_at, NOW + 1);

        let restated = update(
            &db,
            w.project,
            v.id,
            SavedViewChanges {
                state: Some(state(json!({"filter": "x"}))),
                ..Default::default()
            },
            NOW + 2,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(restated.state, state(json!({"filter": "x"})));
        assert_eq!(restated.updated_at, NOW + 2);
    }

    #[tokio::test]
    async fn same_values_write_nothing() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let v = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Errors",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let empty = update(&db, w.project, v.id, SavedViewChanges::default(), NOW + 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(empty.updated_at, NOW);
        let same = update(
            &db,
            w.project,
            v.id,
            SavedViewChanges {
                name: Some("Errors".to_owned()),
                shared: Some(false),
                ..Default::default()
            },
            NOW + 5,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(same.updated_at, NOW);
    }

    #[tokio::test]
    async fn unsharing_an_orphan_shared_view_is_a_conflict() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let s = insert(
            &db,
            new_view(w.project, w.third, SavedViewKind::Traces, "S", true, false),
            NOW,
        )
        .await;
        let third = w.third;
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("DELETE FROM users WHERE id = ?")
                    .bind(third.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let result = update(
            &db,
            w.project,
            s.id,
            SavedViewChanges {
                shared: Some(false),
                ..Default::default()
            },
            NOW + 1,
        )
        .await;
        assert!(matches!(result, Err(MetaError::Conflict(UNSHARE_ORPHAN))));
        assert!(stored(&db, w.project, s.id).await.unwrap().shared);
        let deleted = db
            .write(move |conn| Box::pin(async move { delete_view(conn, w.project, s.id).await }))
            .await
            .unwrap();
        assert!(deleted);
    }

    #[tokio::test]
    async fn update_and_delete_of_a_missing_view() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let missing = Uuid::now_v7();
        let updated = update(&db, w.project, missing, SavedViewChanges::default(), NOW)
            .await
            .unwrap();
        assert!(updated.is_none());
        let deleted = db
            .write(move |conn| Box::pin(async move { delete_view(conn, w.project, missing).await }))
            .await
            .unwrap();
        assert!(!deleted);

        let v = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Traces, "v", false, false),
            NOW,
        )
        .await;
        let existing = db
            .write(move |conn| Box::pin(async move { delete_view(conn, w.project, v.id).await }))
            .await
            .unwrap();
        assert!(existing);
        let got = get_visible(db.reader(), w.project, Viewer::User(w.owner), v.id)
            .await
            .unwrap();
        assert_eq!(got, None);
    }

    #[tokio::test]
    async fn find_by_name_puts_the_viewers_own_first() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let theirs = insert(
            &db,
            new_view(
                w.project,
                w.other,
                SavedViewKind::Traces,
                "Errors",
                true,
                false,
            ),
            NOW,
        )
        .await;
        let mine = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Errors",
                false,
                false,
            ),
            NOW,
        )
        .await;
        insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "errors",
                true,
                false,
            ),
            NOW,
        )
        .await;

        let for_owner = find_visible_by_name(
            db.reader(),
            w.project,
            Viewer::User(w.owner),
            SavedViewKind::Traces,
            "Errors",
        )
        .await
        .unwrap();
        assert_eq!(ids(&for_owner), vec![mine.id, theirs.id]);

        let for_third = find_visible_by_name(
            db.reader(),
            w.project,
            Viewer::User(w.third),
            SavedViewKind::Traces,
            "Errors",
        )
        .await
        .unwrap();
        assert_eq!(ids(&for_third), vec![theirs.id]);

        let for_key = find_visible_by_name(
            db.reader(),
            w.project,
            Viewer::Key,
            SavedViewKind::Traces,
            "Errors",
        )
        .await
        .unwrap();
        assert_eq!(ids(&for_key), vec![theirs.id]);
    }

    #[tokio::test]
    async fn owner_cap_is_a_conflict() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let (project, owner) = (w.project, w.owner);
        db.write(move |conn| {
            Box::pin(async move {
                for index in 0..MAX_VIEWS_PER_OWNER {
                    let (kind, shared) = if index % 2 == 0 {
                        (SavedViewKind::Traces, false)
                    } else {
                        (SavedViewKind::Sql, true)
                    };
                    let view = new_view(
                        project,
                        owner,
                        kind,
                        &format!("view {index}"),
                        shared,
                        false,
                    );
                    insert_view(conn, view, NOW).await?;
                }
                Ok(())
            })
        })
        .await
        .unwrap();

        let one_more = db
            .write(move |conn| {
                Box::pin(async move {
                    let view = new_view(
                        project,
                        owner,
                        SavedViewKind::Traces,
                        "one more",
                        false,
                        false,
                    );
                    insert_view(conn, view, NOW).await
                })
            })
            .await;
        assert!(matches!(
            one_more,
            Err(MetaError::Conflict("too many saved views"))
        ));

        insert(
            &db,
            new_view(
                w.project,
                w.other,
                SavedViewKind::Traces,
                "theirs",
                false,
                false,
            ),
            NOW,
        )
        .await;
        insert(
            &db,
            new_view(
                w.second,
                w.owner,
                SavedViewKind::Traces,
                "elsewhere",
                false,
                false,
            ),
            NOW,
        )
        .await;
    }

    #[tokio::test]
    async fn corrupt_columns_are_reported() {
        // One fresh database per case: a corrupt row would fail every later list too.
        let cases: [(&'static str, &'static str); 4] = [
            ("UPDATE saved_views SET kind = 'bogus' WHERE id = ?", "kind"),
            (
                "UPDATE saved_views SET state = 'not json' WHERE id = ?",
                "state",
            ),
            ("UPDATE saved_views SET state = '[]' WHERE id = ?", "state"),
            (
                "UPDATE saved_views SET id = 'not-a-uuid' WHERE id = ?",
                "id",
            ),
        ];
        for (sql, column) in cases {
            let (_dir, db) = crate::meta::test_db().await;
            let w = world(&db).await;
            let view = insert(
                &db,
                new_view(
                    w.project,
                    w.owner,
                    SavedViewKind::Traces,
                    "Corrupt",
                    true,
                    false,
                ),
                NOW,
            )
            .await;
            let id = view.id.to_string();
            db.write(move |conn| {
                Box::pin(async move {
                    sqlx::query(sql).bind(id).execute(&mut *conn).await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
            match list_visible(db.reader(), w.project, Viewer::Key, None, 10, None).await {
                Err(MetaError::Corrupt { table, column: got }) => {
                    assert_eq!(table, "saved_views");
                    assert_eq!(got, column);
                }
                other => panic!("{sql}: {other:?}"),
            }
        }

        // The owner id must reference a user, so the user row gets a non-UUID id first.
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let view = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "Corrupt",
                true,
                false,
            ),
            NOW,
        )
        .await;
        let view_id = view.id.to_string();
        db.write(move |conn| {
            Box::pin(async move {
                let bad = users::insert_user(conn, new_user("bad@example.com"), NOW)
                    .await?
                    .id;
                sqlx::query("UPDATE users SET id = 'not-a-uuid' WHERE id = ?")
                    .bind(bad.to_string())
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("UPDATE saved_views SET owner_user_id = 'not-a-uuid' WHERE id = ?")
                    .bind(view_id)
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        match list_visible(db.reader(), w.project, Viewer::Key, None, 10, None).await {
            Err(MetaError::Corrupt { table, column }) => {
                assert_eq!(table, "saved_views");
                assert_eq!(column, "owner_user_id");
            }
            other => panic!("owner: {other:?}"),
        }
    }

    #[tokio::test]
    async fn kind_text_round_trips() {
        for kind in [
            SavedViewKind::Traces,
            SavedViewKind::Spans,
            SavedViewKind::Sessions,
            SavedViewKind::Users,
            SavedViewKind::Sql,
        ] {
            assert_eq!(parse_kind(kind_str(kind)), Some(kind));
        }
        assert_eq!(parse_kind("trace"), None);
    }

    #[tokio::test]
    async fn visible_to_agrees_with_list_visible() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let p_owner = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "p o",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let s_owner = insert(
            &db,
            new_view(
                w.project,
                w.owner,
                SavedViewKind::Traces,
                "s o",
                true,
                false,
            ),
            NOW,
        )
        .await;
        let p_other = insert(
            &db,
            new_view(
                w.project,
                w.other,
                SavedViewKind::Traces,
                "p t",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let s_other = insert(
            &db,
            new_view(
                w.project,
                w.other,
                SavedViewKind::Traces,
                "s t",
                true,
                false,
            ),
            NOW,
        )
        .await;
        let p_orphan = insert(
            &db,
            new_view(
                w.project,
                w.third,
                SavedViewKind::Traces,
                "p x",
                false,
                false,
            ),
            NOW,
        )
        .await;
        let s_orphan = insert(
            &db,
            new_view(
                w.project,
                w.third,
                SavedViewKind::Traces,
                "s x",
                true,
                false,
            ),
            NOW,
        )
        .await;
        let third = w.third;
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("DELETE FROM users WHERE id = ?")
                    .bind(third.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let mut rows = Vec::new();
        for id in [
            p_owner.id,
            s_owner.id,
            p_other.id,
            s_other.id,
            p_orphan.id,
            s_orphan.id,
        ] {
            rows.push(stored(&db, w.project, id).await.unwrap());
        }
        for viewer in [Viewer::User(w.owner), Viewer::User(w.other), Viewer::Key] {
            let page = list_visible(db.reader(), w.project, viewer, None, 100, None)
                .await
                .unwrap();
            let listed = sorted(ids(&page.rows));
            let expected = sorted(
                rows.iter()
                    .filter(|row| visible_to(row, viewer))
                    .map(|row| row.id)
                    .collect(),
            );
            assert!(!expected.is_empty());
            assert_eq!(listed, expected);
        }
    }

    #[tokio::test]
    async fn rows_convert_to_the_api_type() {
        let (_dir, db) = crate::meta::test_db().await;
        let w = world(&db).await;
        let row = insert(
            &db,
            new_view(w.project, w.owner, SavedViewKind::Sql, "Slow", true, true),
            NOW,
        )
        .await;
        let api = SavedView::from(row.clone());
        assert_eq!(api.id, row.id);
        assert_eq!(api.kind, SavedViewKind::Sql);
        assert_eq!(api.name, "Slow");
        assert_eq!(api.state, row.state);
        assert_eq!(api.owner_user_id, Some(w.owner));
        assert!(api.shared);
        assert!(api.is_default);
        assert_eq!(api.created_at, NOW);
        assert_eq!(api.updated_at, NOW);
    }
}
