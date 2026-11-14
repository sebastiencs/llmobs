//! Saved list views and SQL queries (spec 09 § Observability data, D-H-6).

use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::api::common::{Page, ProblemCode};
use crate::api::saved_views::{CreateSavedView, SavedView, SavedViewListParams, UpdateSavedView};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::{ProjectPerm, Role};
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError, field_error};
use crate::meta::saved_views::{
    self as store, NewSavedView, SavedViewChanges, SavedViewRow, Viewer, kind_str, visible_to,
};
use crate::routes::read::page_limit;

/// Saved views keep small UI state (D-H-6 caps `state` at 64 KiB).
const SAVED_VIEW_BODY_LIMIT: usize = 128 * 1024;
/// Longest `state` JSON, in bytes.
const MAX_STATE_BYTES: usize = 64 * 1024;
/// Longest name, in characters, after trimming.
const MAX_NAME_CHARS: usize = 200;

/// Saved view routes. Reads and writes need project view: the access layer refuses read-only
/// PATs and every key on writes, and the handlers apply D-H-6. All are documented.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/saved-views",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_views),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/saved-views",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: post(create_view).layer(DefaultBodyLimit::max(SAVED_VIEW_BODY_LIMIT)),
        },
        RouteDef {
            method: Method::PATCH,
            path: "/api/v1/projects/{project}/saved-views/{id}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: patch(update_view).layer(DefaultBodyLimit::max(SAVED_VIEW_BODY_LIMIT)),
        },
        RouteDef {
            method: Method::DELETE,
            path: "/api/v1/projects/{project}/saved-views/{id}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: delete(delete_view),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct ViewPath {
    id: Uuid,
}

/// What the access check decided. `Hidden` answers 404, `Forbidden` 403.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Allowed,
    Hidden,
    Forbidden,
}

/// What a write transaction did. Refusals write nothing.
enum Outcome {
    Missing,
    Forbidden,
    Done(SavedViewRow),
}

async fn list_views(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<SavedViewListParams>,
) -> Result<error::Json<Page<SavedView>>, ApiError> {
    let cursor = checked_cursor(params.cursor.as_deref())?;
    let page = store::list_visible(
        state.meta.reader(),
        access.project.id,
        viewer(&principal),
        params.kind,
        page_limit(params.limit),
        cursor.as_deref(),
    )
    .await?;
    Ok(error::Json(Page {
        data: page.rows.into_iter().map(SavedView::from).collect(),
        next_cursor: page.next_cursor,
    }))
}

async fn create_view(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateSavedView>,
) -> Result<Response, ApiError> {
    let user = principal::session_user(&principal)?.id;
    let role = role_of(&access)?;
    let name = checked_name(&body.name)?;
    checked_state(&body.state)?;
    if body.shared && role < Role::Member {
        return Err(forbidden());
    }
    let project_id = access.project.id;
    let now = state.clock.now_ns();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                store::insert_view(
                    conn,
                    NewSavedView {
                        project_id,
                        kind: body.kind,
                        name,
                        state: body.state,
                        owner_user_id: user,
                        shared: body.shared,
                        is_default: body.is_default,
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    tracing::info!(
        project_id = %project_id,
        view_id = %row.id,
        kind = kind_str(row.kind),
        "saved view created"
    );
    Ok((StatusCode::CREATED, error::Json(SavedView::from(row))).into_response())
}

async fn update_view(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<ViewPath>,
    error::Json(body): error::Json<UpdateSavedView>,
) -> Result<error::Json<SavedView>, ApiError> {
    let user = principal::session_user(&principal)?.id;
    let role = role_of(&access)?;
    let name = body.name.as_deref().map(checked_name).transpose()?;
    if let Some(map) = &body.state {
        checked_state(map)?;
    }
    let changes = SavedViewChanges {
        name,
        state: body.state,
        shared: body.shared,
        is_default: body.is_default,
    };
    let project = access.project.id;
    let id = path.id;
    let now = state.clock.now_ns();
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(row) = store::view_by_id(conn, project, id).await? else {
                    return Ok(Outcome::Missing);
                };
                match may_update(&row, user, role, &changes) {
                    Decision::Hidden => Ok(Outcome::Missing),
                    Decision::Forbidden => Ok(Outcome::Forbidden),
                    Decision::Allowed => Ok(
                        match store::update_view(conn, project, id, changes, now).await? {
                            Some(row) => Outcome::Done(row),
                            None => Outcome::Missing,
                        },
                    ),
                }
            })
        })
        .await?;
    match outcome {
        Outcome::Missing => Err(ApiError::NotFound),
        Outcome::Forbidden => Err(forbidden()),
        Outcome::Done(row) => {
            tracing::info!(
                project_id = %project,
                view_id = %row.id,
                kind = kind_str(row.kind),
                "saved view updated"
            );
            Ok(error::Json(SavedView::from(row)))
        }
    }
}

async fn delete_view(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<ViewPath>,
) -> Result<StatusCode, ApiError> {
    let user = principal::session_user(&principal)?.id;
    let role = role_of(&access)?;
    let project = access.project.id;
    let id = path.id;
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(row) = store::view_by_id(conn, project, id).await? else {
                    return Ok(Outcome::Missing);
                };
                match may_delete(&row, user, role) {
                    Decision::Hidden => Ok(Outcome::Missing),
                    Decision::Forbidden => Ok(Outcome::Forbidden),
                    Decision::Allowed => {
                        if store::delete_view(conn, project, id).await? {
                            Ok(Outcome::Done(row))
                        } else {
                            Ok(Outcome::Missing)
                        }
                    }
                }
            })
        })
        .await?;
    match outcome {
        Outcome::Missing => Err(ApiError::NotFound),
        Outcome::Forbidden => Err(forbidden()),
        Outcome::Done(row) => {
            tracing::info!(
                project_id = %project,
                view_id = %row.id,
                kind = kind_str(row.kind),
                "saved view deleted"
            );
            Ok(StatusCode::NO_CONTENT)
        }
    }
}

/// A user sees their own and shared views; a key sees shared views.
fn viewer(principal: &Principal) -> Viewer {
    match principal {
        Principal::Session { user, .. } | Principal::Pat { user, .. } => Viewer::User(user.id),
        Principal::ApiKey { .. } => Viewer::Key,
    }
}

/// The caller's effective role. Keys have none and cannot write saved views (403).
///
/// Writes cannot reach here with a key (the access layer refuses keys on POST, PATCH and
/// DELETE with `ProjectPerm::View`); this keeps the handler correct on its own.
fn role_of(access: &ProjectAccess) -> Result<Role, ApiError> {
    access.role.ok_or_else(forbidden)
}

/// Trimmed name, 1–200 characters.
fn checked_name(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim();
    let length = trimmed.chars().count();
    if (1..=MAX_NAME_CHARS).contains(&length) {
        Ok(trimmed.to_owned())
    } else {
        Err(field_error("name", "name must be 1 to 200 characters"))
    }
}

/// `state` serializes to at most 64 KiB.
fn checked_state(state: &Map<String, Value>) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec(state).map_err(|error| ApiError::Internal(Box::new(error)))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(field_error("state", "state must be at most 65536 bytes"));
    }
    Ok(())
}

/// A cursor is the id of the previous page's last view.
fn checked_cursor(cursor: Option<&str>) -> Result<Option<String>, ApiError> {
    cursor
        .map(|text| {
            // Ids are stored canonically, so an uppercase cursor must compare the same way.
            Uuid::parse_str(text)
                .map(|id| id.to_string())
                .map_err(|_| field_error("cursor", "invalid cursor"))
        })
        .transpose()
}

fn forbidden() -> ApiError {
    ApiError::Forbidden {
        code: ProblemCode::Forbidden,
    }
}

/// D-H-6 for PATCH.
fn may_update(row: &SavedViewRow, user: Uuid, role: Role, changes: &SavedViewChanges) -> Decision {
    if !visible_to(row, Viewer::User(user)) {
        return Decision::Hidden;
    }
    if row.owner_user_id == Some(user) {
        let empty = changes.name.is_none()
            && changes.state.is_none()
            && changes.shared.is_none()
            && changes.is_default.is_none();
        if empty {
            return Decision::Allowed;
        }
        // Sharing, or changing a view that stays shared, needs member.
        let shared_after = changes.shared.unwrap_or(row.shared);
        return if shared_after && role < Role::Member {
            Decision::Forbidden
        } else {
            Decision::Allowed
        };
    }
    // Not the owner, so the row is shared. Only a project admin may unshare it.
    let unshare_only =
        changes.name.is_none() && changes.state.is_none() && changes.is_default.is_none();
    if role == Role::Admin && changes.shared == Some(false) && unshare_only {
        Decision::Allowed
    } else {
        Decision::Forbidden
    }
}

/// D-H-6 for DELETE.
fn may_delete(row: &SavedViewRow, user: Uuid, role: Role) -> Decision {
    if !visible_to(row, Viewer::User(user)) {
        return Decision::Hidden;
    }
    if row.owner_user_id == Some(user) || (role == Role::Admin && row.shared) {
        Decision::Allowed
    } else {
        Decision::Forbidden
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::api::saved_views::SavedViewKind;
    use crate::auth::roles::OrgRole;
    use crate::meta::MetaError;
    use crate::test_support::{Seeded, TestApp, capture_logs};

    /// `2026-01-01T00:00:00Z`, the manual clock's start.
    const START_NS: i64 = 1_767_225_600_000_000_000;
    const ONE_SECOND_NS: i64 = 1_000_000_000;

    /// The tests of this module run one at a time. A log capture only sees an event when the
    /// event's callsite registered while the capture was the thread's subscriber, and a
    /// callsite registers on its first hit in any thread. A test running beside the capture
    /// could register the handler's callsites without it, and the capture would miss them.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        SERIAL.lock().await
    }

    fn row(owner: Option<Uuid>, shared: bool) -> SavedViewRow {
        SavedViewRow {
            id: Uuid::now_v7(),
            kind: SavedViewKind::Traces,
            name: "Errors".to_owned(),
            state: Map::new(),
            owner_user_id: owner,
            shared,
            is_default: false,
            created_at: START_NS,
            updated_at: START_NS,
        }
    }

    fn views_path(seeded: &Seeded) -> String {
        format!("/api/v1/projects/{}/saved-views", seeded.project.id)
    }

    /// Sends one request with a session cookie, and a JSON body when `body` is given.
    async fn call(
        app: &TestApp,
        method: Method,
        path: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> crate::test_support::TestResponse {
        let request = app.request(method, path).header("cookie", cookie);
        match body {
            Some(body) => request.json(&body).send().await,
            None => request.send().await,
        }
    }

    async fn create(
        app: &TestApp,
        cookie: &str,
        base: &str,
        body: Value,
    ) -> crate::test_support::TestResponse {
        call(app, Method::POST, base, cookie, Some(body)).await
    }

    /// The `id` of a created view.
    fn id_of(response: &crate::test_support::TestResponse) -> Uuid {
        response.json::<SavedView>().id
    }

    fn list_of(response: &crate::test_support::TestResponse) -> Page<SavedView> {
        assert_eq!(response.status, StatusCode::OK);
        response.json()
    }

    fn names(page: &Page<SavedView>) -> BTreeSet<String> {
        page.data.iter().map(|view| view.name.clone()).collect()
    }

    /// Moves a view to `user` as its owner, as a migration of old data would.
    async fn reassign(app: &TestApp, view: Uuid, user: Uuid) {
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE saved_views SET owner_user_id = ? WHERE id = ?")
                        .bind(user.to_string())
                        .bind(view.to_string())
                        .execute(&mut *conn)
                        .await
                        .map_err(MetaError::Db)?;
                    Ok(())
                })
            })
            .await
            .unwrap();
    }

    fn update_changes(
        name: Option<&str>,
        state: Option<Map<String, Value>>,
        shared: Option<bool>,
        is_default: Option<bool>,
    ) -> SavedViewChanges {
        SavedViewChanges {
            name: name.map(str::to_owned),
            state,
            shared,
            is_default,
        }
    }

    #[test]
    fn update_rules() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let private = row(Some(a), false);
        let shared = row(Some(a), true);
        let name = update_changes(Some("x"), None, None, None);
        let shared_true = update_changes(None, None, Some(true), None);
        let shared_false = update_changes(None, None, Some(false), None);
        let default_true = update_changes(None, None, None, Some(true));
        let empty = update_changes(None, None, None, None);
        let unshare_named = update_changes(Some("x"), None, Some(false), None);
        let unshare_default = update_changes(None, None, Some(false), Some(false));
        let cases = [
            (&private, a, Role::Viewer, &name, Decision::Allowed),
            (&private, a, Role::Viewer, &shared_true, Decision::Forbidden),
            (&private, a, Role::Member, &shared_true, Decision::Allowed),
            (&shared, a, Role::Viewer, &name, Decision::Forbidden),
            (&shared, a, Role::Viewer, &default_true, Decision::Forbidden),
            (&shared, a, Role::Viewer, &shared_false, Decision::Allowed),
            (&shared, a, Role::Viewer, &empty, Decision::Allowed),
            (&private, b, Role::Admin, &shared_false, Decision::Hidden),
            (&shared, b, Role::Admin, &shared_false, Decision::Allowed),
            (&shared, b, Role::Admin, &unshare_named, Decision::Forbidden),
            (&shared, b, Role::Admin, &empty, Decision::Forbidden),
            (
                &shared,
                b,
                Role::Admin,
                &unshare_default,
                Decision::Forbidden,
            ),
            (&shared, b, Role::Member, &shared_false, Decision::Forbidden),
        ];
        for (view, user, role, changes, want) in cases {
            assert_eq!(may_update(view, user, role, changes), want, "{role:?}");
        }
    }

    #[test]
    fn delete_rules() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let private = row(Some(a), false);
        let shared = row(Some(a), true);
        assert_eq!(may_delete(&private, a, Role::Viewer), Decision::Allowed);
        assert_eq!(may_delete(&shared, a, Role::Viewer), Decision::Allowed);
        assert_eq!(may_delete(&private, b, Role::Admin), Decision::Hidden);
        assert_eq!(may_delete(&shared, b, Role::Admin), Decision::Allowed);
        assert_eq!(may_delete(&shared, b, Role::Member), Decision::Forbidden);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_list_update_delete_round_trip() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let base = views_path(&seeded);
        let response = create(
            &app,
            &seeded.cookie,
            &base,
            json!({"kind": "traces", "name": "  Errors  ", "state": {"filter": "status = 'error'", "range": "24h"}}),
        )
        .await;
        assert_eq!(response.status, StatusCode::CREATED);
        let created: SavedView = response.json();
        assert_eq!(created.name, "Errors");
        assert_eq!(created.owner_user_id, Some(seeded.user_id));
        assert!(!created.shared);
        assert!(!created.is_default);
        assert_eq!(created.created_at, START_NS);
        assert_eq!(created.updated_at, START_NS);

        let listed = list_of(
            &call(
                &app,
                Method::GET,
                &format!("{base}?kind=traces"),
                &seeded.cookie,
                None,
            )
            .await,
        );
        assert_eq!(listed.data.len(), 1);
        assert_eq!(listed.next_cursor, None);

        app.state.clock.advance(Duration::from_secs(1));
        let item = format!("{base}/{}", created.id);
        let updated = call(
            &app,
            Method::PATCH,
            &item,
            &seeded.cookie,
            Some(json!({"name": "Errors only"})),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK);
        let updated: SavedView = updated.json();
        assert_eq!(updated.name, "Errors only");
        assert_eq!(updated.created_at, START_NS);
        assert_eq!(updated.updated_at, START_NS + ONE_SECOND_NS);

        // An empty PATCH by the owner changes nothing, so `updated_at` stays.
        let same = call(&app, Method::PATCH, &item, &seeded.cookie, Some(json!({}))).await;
        assert_eq!(same.status, StatusCode::OK);
        assert_eq!(
            same.json::<SavedView>().updated_at,
            START_NS + ONE_SECOND_NS
        );

        let deleted = call(&app, Method::DELETE, &item, &seeded.cookie, None).await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        let after = list_of(&call(&app, Method::GET, &base, &seeded.cookie, None).await);
        assert!(after.data.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn visibility_by_caller() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let base = views_path(&seeded);
        for body in [
            json!({"kind": "traces", "name": "private", "state": {}}),
            json!({"kind": "traces", "name": "shared traces", "state": {}, "shared": true}),
            json!({"kind": "sql", "name": "shared sql", "state": {}, "shared": true}),
        ] {
            assert_eq!(
                create(&app, &seeded.cookie, &base, body).await.status,
                StatusCode::CREATED
            );
        }
        let shared = BTreeSet::from(["shared traces".to_owned(), "shared sql".to_owned()]);

        let as_member = list_of(&call(&app, Method::GET, &base, &member.cookie, None).await);
        assert_eq!(names(&as_member), shared);

        let key = app
            .request(Method::GET, &base)
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .send()
            .await;
        assert_eq!(names(&list_of(&key)), shared);
        let key_sql = app
            .request(Method::GET, &format!("{base}?kind=sql"))
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .send()
            .await;
        assert_eq!(
            names(&list_of(&key_sql)),
            BTreeSet::from(["shared sql".to_owned()])
        );

        let read_pat = app.personal_token(member.id, false).await;
        let as_pat = app
            .request(Method::GET, &base)
            .header("authorization", &format!("Bearer {read_pat}"))
            .send()
            .await;
        assert_eq!(names(&list_of(&as_pat)), shared);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sharing_needs_member() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let base = views_path(&seeded);
        let refused = [
            json!({"kind": "traces", "name": "x", "state": {}, "shared": true}),
            json!({"kind": "traces", "name": "x", "state": {}, "shared": true, "is_default": true}),
        ];
        for body in refused {
            let response = create(&app, &viewer.cookie, &base, body).await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
            assert_eq!(response.problem().code, ProblemCode::Forbidden);
        }
        let allowed = [
            json!({"kind": "traces", "name": "private", "state": {}}),
            json!({"kind": "traces", "name": "private default", "state": {}, "is_default": true}),
        ];
        for body in allowed {
            assert_eq!(
                create(&app, &viewer.cookie, &base, body).await.status,
                StatusCode::CREATED
            );
        }
        let shared = create(
            &app,
            &member.cookie,
            &base,
            json!({"kind": "traces", "name": "shared", "state": {}, "shared": true}),
        )
        .await;
        assert_eq!(shared.status, StatusCode::CREATED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_the_owner_edits() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let admin = app
            .add_member(&seeded, "admin@example.com", OrgRole::Admin)
            .await;
        let base = views_path(&seeded);
        let shared = id_of(
            &create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": "shared", "state": {}, "shared": true}),
            )
            .await,
        );
        let item = format!("{base}/{shared}");
        let refused = [
            (&member.cookie, Method::PATCH, json!({"name": "x"})),
            (&member.cookie, Method::PATCH, json!({})),
            (&admin.cookie, Method::PATCH, json!({"name": "x"})),
        ];
        for (cookie, method, body) in refused {
            let response = call(&app, method, &item, cookie, Some(body)).await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
        }
        assert_eq!(
            call(&app, Method::DELETE, &item, &member.cookie, None)
                .await
                .status,
            StatusCode::FORBIDDEN
        );

        let owner_default = call(
            &app,
            Method::PATCH,
            &item,
            &seeded.cookie,
            Some(json!({"is_default": true})),
        )
        .await;
        assert_eq!(owner_default.status, StatusCode::OK);

        let unshared = call(
            &app,
            Method::PATCH,
            &item,
            &admin.cookie,
            Some(json!({"shared": false})),
        )
        .await;
        assert_eq!(unshared.status, StatusCode::OK);
        let unshared: SavedView = unshared.json();
        assert!(!unshared.shared);
        assert!(!unshared.is_default);

        let owner_list = list_of(&call(&app, Method::GET, &base, &seeded.cookie, None).await);
        assert!(owner_list.data.iter().any(|view| view.id == shared));
        let member_list = list_of(&call(&app, Method::GET, &base, &member.cookie, None).await);
        assert!(!member_list.data.iter().any(|view| view.id == shared));

        let second = id_of(
            &create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": "second", "state": {}, "shared": true}),
            )
            .await,
        );
        let moderated = call(
            &app,
            Method::DELETE,
            &format!("{base}/{second}"),
            &admin.cookie,
            None,
        )
        .await;
        assert_eq!(moderated.status, StatusCode::NO_CONTENT);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_viewer_owner_of_a_shared_view() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let base = views_path(&seeded);
        let shared = id_of(
            &create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": "shared", "state": {}, "shared": true}),
            )
            .await,
        );
        reassign(&app, shared, viewer.id).await;
        let item = format!("{base}/{shared}");
        for body in [json!({"name": "x"}), json!({"is_default": true})] {
            let response = call(&app, Method::PATCH, &item, &viewer.cookie, Some(body)).await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
        }
        assert_eq!(
            call(&app, Method::PATCH, &item, &viewer.cookie, Some(json!({})))
                .await
                .status,
            StatusCode::OK
        );
        let unshared = call(
            &app,
            Method::PATCH,
            &item,
            &viewer.cookie,
            Some(json!({"shared": false})),
        )
        .await;
        assert_eq!(unshared.status, StatusCode::OK);
        assert!(!unshared.json::<SavedView>().shared);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_viewer_owner_deletes_their_shared_view() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let base = views_path(&seeded);
        let shared = id_of(
            &create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": "shared", "state": {}, "shared": true}),
            )
            .await,
        );
        reassign(&app, shared, viewer.id).await;
        let deleted = call(
            &app,
            Method::DELETE,
            &format!("{base}/{shared}"),
            &viewer.cookie,
            None,
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn private_views_are_not_found_for_others() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let admin = app
            .add_member(&seeded, "admin@example.com", OrgRole::Admin)
            .await;
        let base = views_path(&seeded);
        let private = id_of(
            &create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": "private", "state": {}}),
            )
            .await,
        );
        let item = format!("{base}/{private}");
        let patched = call(
            &app,
            Method::PATCH,
            &item,
            &member.cookie,
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(patched.status, StatusCode::NOT_FOUND);
        assert_eq!(
            call(&app, Method::DELETE, &item, &member.cookie, None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, Method::DELETE, &item, &admin.cookie, None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&app, Method::PATCH, &item, &admin.cookie, Some(json!({})))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_and_foreign_ids_are_not_found() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (other, _key) = app.add_project(&seeded, "other-bot", None).await;
        let base = views_path(&seeded);
        let other_base = format!("/api/v1/projects/{}/saved-views", other.id);
        let foreign = id_of(
            &create(
                &app,
                &seeded.cookie,
                &other_base,
                json!({"kind": "traces", "name": "elsewhere", "state": {}}),
            )
            .await,
        );
        let absent = format!("{base}/01900000-0000-7000-8000-000000009999");
        let foreign_item = format!("{base}/{foreign}");
        for path in [&absent, &foreign_item] {
            assert_eq!(
                call(&app, Method::PATCH, path, &seeded.cookie, Some(json!({})))
                    .await
                    .status,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                call(&app, Method::DELETE, path, &seeded.cookie, None)
                    .await
                    .status,
                StatusCode::NOT_FOUND
            );
        }
        let malformed = call(
            &app,
            Method::PATCH,
            &format!("{base}/not-a-uuid"),
            &seeded.cookie,
            Some(json!({})),
        )
        .await;
        assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn defaults_follow_their_scope() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let base = views_path(&seeded);
        create(
            &app,
            &seeded.cookie,
            &base,
            json!({"kind": "traces", "name": "s1", "state": {}, "shared": true, "is_default": true}),
        )
        .await;
        create(
            &app,
            &member.cookie,
            &base,
            json!({"kind": "traces", "name": "s2", "state": {}, "shared": true, "is_default": true}),
        )
        .await;
        let flags = |page: &Page<SavedView>| -> BTreeSet<(String, bool)> {
            page.data
                .iter()
                .map(|view| (view.name.clone(), view.is_default))
                .collect()
        };
        let after_shared = list_of(&call(&app, Method::GET, &base, &seeded.cookie, None).await);
        assert_eq!(
            flags(&after_shared),
            BTreeSet::from([("s1".to_owned(), false), ("s2".to_owned(), true)])
        );

        create(
            &app,
            &seeded.cookie,
            &base,
            json!({"kind": "traces", "name": "p", "state": {}, "is_default": true}),
        )
        .await;
        let p2 = id_of(
            &create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": "p2", "state": {}}),
            )
            .await,
        );
        let promoted = call(
            &app,
            Method::PATCH,
            &format!("{base}/{p2}"),
            &seeded.cookie,
            Some(json!({"is_default": true})),
        )
        .await;
        assert_eq!(promoted.status, StatusCode::OK);

        let after_private = list_of(&call(&app, Method::GET, &base, &seeded.cookie, None).await);
        assert_eq!(
            flags(&after_private),
            BTreeSet::from([
                ("s1".to_owned(), false),
                ("s2".to_owned(), true),
                ("p".to_owned(), false),
                ("p2".to_owned(), true),
            ])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn orphan_shared_views_are_deleted_not_unshared() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let admin = app
            .add_member(&seeded, "admin@example.com", OrgRole::Admin)
            .await;
        let base = views_path(&seeded);
        let shared = id_of(
            &create(
                &app,
                &member.cookie,
                &base,
                json!({"kind": "traces", "name": "orphan", "state": {}, "shared": true}),
            )
            .await,
        );
        let member_id = member.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("DELETE FROM users WHERE id = ?")
                        .bind(member_id.to_string())
                        .execute(&mut *conn)
                        .await
                        .map_err(MetaError::Db)?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let item = format!("{base}/{shared}");
        let unshare = call(
            &app,
            Method::PATCH,
            &item,
            &admin.cookie,
            Some(json!({"shared": false})),
        )
        .await;
        assert_eq!(unshare.status, StatusCode::CONFLICT);
        assert_eq!(
            unshare.problem().detail.as_deref(),
            Some("the view has no owner; delete it instead")
        );
        assert_eq!(
            call(&app, Method::DELETE, &item, &admin.cookie, None)
                .await
                .status,
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keys_and_read_tokens_cannot_write() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let base = views_path(&seeded);
        let body = json!({"kind": "traces", "name": "x", "state": {}});

        let read_key = app
            .request(Method::POST, &base)
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .json(&body)
            .send()
            .await;
        assert_eq!(read_key.status, StatusCode::FORBIDDEN);
        assert_eq!(read_key.header("x-test-access"), Some("denied-403"));

        let read_pat = app.personal_token(member.id, false).await;
        let read_pat = app
            .request(Method::POST, &base)
            .header("authorization", &format!("Bearer {read_pat}"))
            .json(&body)
            .send()
            .await;
        assert_eq!(read_pat.status, StatusCode::FORBIDDEN);

        let write_pat = app.personal_token(member.id, true).await;
        let bearer = format!("Bearer {write_pat}");
        let created = app
            .request(Method::POST, &base)
            .header("authorization", &bearer)
            .json(&body)
            .send()
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
        let created: SavedView = created.json();
        assert_eq!(created.owner_user_id, Some(member.id));
        let item = format!("{base}/{}", created.id);
        let patched = app
            .request(Method::PATCH, &item)
            .header("authorization", &bearer)
            .json(&json!({"name": "y"}))
            .send()
            .await;
        assert_eq!(patched.status, StatusCode::OK);
        let deleted = app
            .request(Method::DELETE, &item)
            .header("authorization", &bearer)
            .send()
            .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn validation_errors_name_the_field() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let base = views_path(&seeded);
        let long = "a".repeat(201);
        let big_state = json!({"s": "x".repeat(70_000)});

        let field_of = |response: &crate::test_support::TestResponse| -> String {
            assert_eq!(response.status, StatusCode::BAD_REQUEST);
            let problem = response.problem();
            assert_eq!(problem.code, ProblemCode::BadRequest);
            problem.errors[0].field.clone()
        };
        let post = |body: Value| {
            let app = &app;
            let cookie = &seeded.cookie;
            let base = &base;
            async move { create(app, cookie, base, body).await }
        };
        assert_eq!(
            field_of(&post(json!({"kind": "traces", "name": "   ", "state": {}})).await),
            "name"
        );
        assert_eq!(
            field_of(&post(json!({"kind": "traces", "name": long, "state": {}})).await),
            "name"
        );
        assert_eq!(
            field_of(&post(json!({"kind": "traces", "name": "x", "state": big_state})).await),
            "state"
        );

        let shared = id_of(
            &post(json!({"kind": "traces", "name": "shared", "state": {}, "shared": true})).await,
        );
        let item = format!("{base}/{shared}");
        let patch = |cookie: &str, body: Value| {
            let app = &app;
            let item = &item;
            let cookie = cookie.to_owned();
            async move { call(app, Method::PATCH, item, &cookie, Some(body)).await }
        };
        assert_eq!(
            field_of(&patch(&seeded.cookie, json!({"name": ""})).await),
            "name"
        );
        assert_eq!(
            field_of(&patch(&seeded.cookie, json!({"name": "a".repeat(201)})).await),
            "name"
        );
        assert_eq!(
            field_of(&patch(&seeded.cookie, json!({"state": {"s": "x".repeat(70_000)}})).await),
            "state"
        );
        assert_eq!(
            patch(&seeded.cookie, json!({"state": []})).await.status,
            StatusCode::BAD_REQUEST
        );
        // Validation comes before ownership: a member's bad name is 400, not 403.
        assert_eq!(
            field_of(&patch(&member.cookie, json!({"name": ""})).await),
            "name"
        );
        assert_eq!(
            call(
                &app,
                Method::GET,
                &format!("{base}?cursor=nope"),
                &seeded.cookie,
                None,
            )
            .await
            .problem()
            .errors[0]
                .field,
            "cursor"
        );
        assert_eq!(
            call(
                &app,
                Method::GET,
                &format!("{base}?kind=bogus"),
                &seeded.cookie,
                None,
            )
            .await
            .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                Method::GET,
                &format!("{base}?limit=0"),
                &seeded.cookie,
                None
            )
            .await
            .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                Method::GET,
                &format!("{base}?limit=1001"),
                &viewer.cookie,
                None
            )
            .await
            .status,
            StatusCode::BAD_REQUEST
        );

        // Boundaries that pass.
        let exact = "a".repeat(200);
        assert_eq!(
            post(json!({"kind": "traces", "name": exact, "state": {}}))
                .await
                .status,
            StatusCode::CREATED
        );
        let padded = format!("  {}  ", "b".repeat(200));
        let trimmed = post(json!({"kind": "traces", "name": padded, "state": {}})).await;
        assert_eq!(trimmed.status, StatusCode::CREATED);
        assert_eq!(trimmed.json::<SavedView>().name, "b".repeat(200));
        let accented = "é".repeat(200);
        assert_eq!(
            post(json!({"kind": "traces", "name": accented, "state": {}}))
                .await
                .status,
            StatusCode::CREATED
        );
        assert_eq!(
            field_of(&post(json!({"kind": "traces", "name": "é".repeat(201), "state": {}})).await),
            "name"
        );
        // `{"s":""}` is 8 bytes, so this filler makes the state exactly 65,536 bytes.
        let filler = "x".repeat(65_536 - r#"{"s":""}"#.len());
        assert_eq!(
            post(json!({"kind": "traces", "name": "edge", "state": {"s": filler}}))
                .await
                .status,
            StatusCode::CREATED
        );

        // Too large and wrong media type.
        let huge =
            post(json!({"kind": "traces", "name": "x", "state": {"s": "x".repeat(140_000)}})).await;
        assert_eq!(huge.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(huge.problem().code, ProblemCode::PayloadTooLarge);
        let no_type = app
            .request(Method::POST, &base)
            .header("cookie", &seeded.cookie)
            .body(serde_json::to_vec(&json!({"kind": "traces", "name": "x", "state": {}})).unwrap())
            .send()
            .await;
        assert_eq!(no_type.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(no_type.problem().code, ProblemCode::UnsupportedMediaType);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn owner_cap_is_conflict() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let owner = seeded.user_id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let now = START_NS;
                    for _ in 0..store::MAX_VIEWS_PER_OWNER {
                        store::insert_view(
                            conn,
                            NewSavedView {
                                project_id: project,
                                kind: SavedViewKind::Traces,
                                name: "bulk".to_owned(),
                                state: Map::new(),
                                owner_user_id: owner,
                                shared: false,
                                is_default: false,
                            },
                            now,
                        )
                        .await?;
                    }
                    Ok(())
                })
            })
            .await
            .unwrap();
        let response = create(
            &app,
            &seeded.cookie,
            &views_path(&seeded),
            json!({"kind": "traces", "name": "one more", "state": {}}),
        )
        .await;
        assert_eq!(response.status, StatusCode::CONFLICT);
        assert_eq!(response.problem().code, ProblemCode::Conflict);
        assert_eq!(
            response.problem().detail.as_deref(),
            Some("too many saved views")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pages_follow_the_cursor() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let base = views_path(&seeded);
        for name in ["one", "two", "three"] {
            create(
                &app,
                &seeded.cookie,
                &base,
                json!({"kind": "traces", "name": name, "state": {}}),
            )
            .await;
        }
        create(
            &app,
            &seeded.cookie,
            &base,
            json!({"kind": "sql", "name": "query", "state": {"query": "SELECT 1"}}),
        )
        .await;

        let first = list_of(
            &call(
                &app,
                Method::GET,
                &format!("{base}?kind=traces&limit=2"),
                &seeded.cookie,
                None,
            )
            .await,
        );
        assert_eq!(first.data.len(), 2);
        let cursor = first.next_cursor.clone().expect("a second page");
        let second = list_of(
            &call(
                &app,
                Method::GET,
                &format!("{base}?kind=traces&limit=2&cursor={cursor}"),
                &seeded.cookie,
                None,
            )
            .await,
        );
        assert_eq!(second.data.len(), 1);
        assert_eq!(second.next_cursor, None);
        let upper = list_of(
            &call(
                &app,
                Method::GET,
                &format!(
                    "{base}?kind=traces&limit=2&cursor={}",
                    cursor.to_uppercase()
                ),
                &seeded.cookie,
                None,
            )
            .await,
        );
        assert_eq!(upper.data, second.data);
        let ids: BTreeSet<Uuid> = first
            .data
            .iter()
            .chain(&second.data)
            .map(|view| view.id)
            .collect();
        assert_eq!(ids.len(), 3);

        let sql = list_of(
            &call(
                &app,
                Method::GET,
                &format!("{base}?kind=sql"),
                &seeded.cookie,
                None,
            )
            .await,
        );
        assert_eq!(names(&sql), BTreeSet::from(["query".to_owned()]));
    }

    #[tokio::test]
    async fn logs_carry_ids_not_names_or_state() {
        let _serial = serial().await;
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let base = views_path(&seeded);
        let body = json!({
            "kind": "traces",
            "name": "Secret view name",
            "state": {"filter": "secret-filter-text"},
        });
        let (ids, logs) = capture_logs(async {
            // Register every callsite of the handlers under this capture (see `SERIAL`).
            tracing::callsite::rebuild_interest_cache();
            let created = create(&app, &seeded.cookie, &base, body.clone()).await;
            assert_eq!(created.status, StatusCode::CREATED);
            let id = id_of(&created);
            let item = format!("{base}/{id}");
            let patched = call(
                &app,
                Method::PATCH,
                &item,
                &seeded.cookie,
                Some(json!({"name": "Secret view name"})),
            )
            .await;
            assert_eq!(patched.status, StatusCode::OK);
            let deleted = call(&app, Method::DELETE, &item, &seeded.cookie, None).await;
            assert_eq!(deleted.status, StatusCode::NO_CONTENT);
            let refused = create(
                &app,
                &viewer.cookie,
                &base,
                json!({
                    "kind": "traces",
                    "name": "Secret view name",
                    "state": {"filter": "secret-filter-text"},
                    "shared": true,
                }),
            )
            .await;
            assert_eq!(refused.status, StatusCode::FORBIDDEN);
            id
        })
        .await;
        assert!(logs.contains("saved view created"), "{logs}");
        assert!(logs.contains("saved view updated"), "{logs}");
        assert!(logs.contains("saved view deleted"), "{logs}");
        assert!(logs.contains(&ids.to_string()), "{logs}");
        assert!(!logs.contains("Secret view name"), "{logs}");
        assert!(!logs.contains("secret-filter-text"), "{logs}");
    }
}
