//! Score definition routes (spec 09 § Evaluation & alerts, spec 04 § Scores definitions,
//! D-I-24, D-I-26). Members and above write; every project reader lists.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use llmobs_core::enums::ScoreDataType;
use llmobs_core::ids::ProjectId;
use serde::Deserialize;
use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::api::common::{FieldError, Page};
use crate::api::score_definitions::{
    CreateScoreDefinitionRequest, ScoreDefinition, ScoreDefinitionCategory,
    ScoreDefinitionListParams, UpdateScoreDefinitionRequest,
};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::MetaError;
use crate::meta::score_definitions::{
    DefinitionCategory, DefinitionFields, DefinitionRow, get_definition, get_definition_tx,
    insert_definition, list_definitions, update_definition,
};

use super::orgs::page_named;

/// Score definition routes. Reads need project view; writes need
/// [`ProjectPerm::ManageEvaluation`] (member and above, D-I-24). Not session-only.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}/score-definitions",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_score_definitions),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/projects/{project}/score-definitions",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: post(create_score_definition),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/projects/{project}/score-definitions/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: patch(update_score_definition),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/projects/{project}/score-definitions/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: delete(archive_score_definition),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct IdPath {
    id: Uuid,
}

async fn list_score_definitions(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<ScoreDefinitionListParams>,
) -> Result<error::Json<Page<ScoreDefinition>>, ApiError> {
    let rows = list_definitions(
        state.meta.reader(),
        access.project.id,
        params.include_archived,
    )
    .await?;
    let items = rows
        .iter()
        .map(|row| (row.name.clone(), row.id.to_string(), definition_json(row)))
        .collect();
    Ok(error::Json(page_named(
        items,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn create_score_definition(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateScoreDefinitionRequest>,
) -> Result<(StatusCode, error::Json<ScoreDefinition>), ApiError> {
    let user = principal::session_user(&principal)?;
    let fields = DefinitionFields {
        name: body.name.trim().to_owned(),
        categories: body
            .categories
            .as_deref()
            .map(to_categories)
            .unwrap_or_default(),
        min_value: body.min_value,
        max_value: body.max_value,
        description: body.description.filter(|text| !text.is_empty()),
        archived_at: None,
    };
    let errors = check_fields(body.data_type, &fields);
    if !errors.is_empty() {
        return Err(bad_request(errors));
    }
    let id = Uuid::now_v7();
    let project = access.project.id;
    let user_id = user.id;
    let data_type = body.data_type;
    let now = state.clock.now_ns();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(
                async move { insert_definition(conn, id, project, data_type, fields, now).await },
            )
        })
        .await?;
    state.definitions.forget(project);
    tracing::info!(
        target: "audit",
        score_definition_id = %id,
        project_id = %project,
        user_id = %user_id,
        "score definition created"
    );
    Ok((StatusCode::CREATED, error::Json(definition_json(&row))))
}

async fn update_score_definition(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
    error::Json(body): error::Json<UpdateScoreDefinitionRequest>,
) -> Result<error::Json<ScoreDefinition>, ApiError> {
    let user = principal::session_user(&principal)?;
    let project = access.project.id;
    let now = state.clock.now_ns();
    // The stored data type decides what is valid, so the row is read first (D-I-26).
    let expected = get_definition(state.meta.reader(), project, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let fields = merged_fields(&expected, &body, now);
    // Report only what the body sent, plus the cross-field rules it can break.
    let errors: Vec<FieldError> = check_fields(expected.data_type, &fields)
        .into_iter()
        .filter(|error| sent(&body, &error.field))
        .collect();
    if !errors.is_empty() {
        return Err(bad_request(errors));
    }
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move { write_update(conn, project, id, &expected, fields, now).await })
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    state.definitions.forget(project);
    tracing::info!(
        target: "audit",
        score_definition_id = %id,
        project_id = %project,
        user_id = %user.id,
        "score definition updated"
    );
    Ok(error::Json(definition_json(&row)))
}

async fn archive_score_definition(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
) -> Result<StatusCode, ApiError> {
    let user = principal::session_user(&principal)?;
    let project = access.project.id;
    let now = state.clock.now_ns();
    let found = state
        .meta
        .write(move |conn| Box::pin(async move { archive_tx(conn, project, id, now).await }))
        .await?;
    if !found {
        return Err(ApiError::NotFound);
    }
    state.definitions.forget(project);
    tracing::info!(
        target: "audit",
        score_definition_id = %id,
        project_id = %project,
        user_id = %user.id,
        "score definition archived"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Step 4 of the update: refuses a row that changed since `expected` was read, then
/// writes `fields`. `Ok(None)` when the row is gone.
async fn write_update(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
    expected: &DefinitionRow,
    fields: DefinitionFields,
    now: i64,
) -> Result<Option<DefinitionRow>, MetaError> {
    let Some(current) = get_definition_tx(conn, project, id).await? else {
        return Ok(None);
    };
    if current != *expected {
        return Err(MetaError::Conflict("score definition changed, retry"));
    }
    update_definition(conn, project, id, fields, now).await
}

/// Archives the definition unless it already is. `false` when it does not exist.
async fn archive_tx(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
    now: i64,
) -> Result<bool, MetaError> {
    let Some(row) = get_definition_tx(conn, project, id).await? else {
        return Ok(false);
    };
    if row.archived_at.is_some() {
        return Ok(true);
    }
    let fields = DefinitionFields {
        archived_at: Some(now),
        ..fields_of(&row)
    };
    Ok(update_definition(conn, project, id, fields, now)
        .await?
        .is_some())
}

fn bad_request(errors: Vec<FieldError>) -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors,
    }
}

fn field_error(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.to_owned(),
        position: None,
    }
}

fn fields_of(row: &DefinitionRow) -> DefinitionFields {
    DefinitionFields {
        name: row.name.clone(),
        categories: row.categories.clone(),
        min_value: row.min_value,
        max_value: row.max_value,
        description: row.description.clone(),
        archived_at: row.archived_at,
    }
}

/// The stored row with every field the PATCH sends replaced. `description: ""` clears it;
/// `archived: true` keeps an earlier `archived_at`.
fn merged_fields(
    row: &DefinitionRow,
    body: &UpdateScoreDefinitionRequest,
    now: i64,
) -> DefinitionFields {
    DefinitionFields {
        name: body
            .name
            .as_deref()
            .map_or_else(|| row.name.clone(), |name| name.trim().to_owned()),
        categories: body
            .categories
            .as_deref()
            .map_or_else(|| row.categories.clone(), to_categories),
        min_value: body.min_value.or(row.min_value),
        max_value: body.max_value.or(row.max_value),
        description: match body.description.as_deref() {
            None => row.description.clone(),
            Some("") => None,
            Some(text) => Some(text.to_owned()),
        },
        archived_at: match body.archived {
            None => row.archived_at,
            Some(true) => row.archived_at.or(Some(now)),
            Some(false) => None,
        },
    }
}

/// Whether a field error belongs to what the PATCH sent. A range error counts when the body
/// sent either bound, because `min_value >= max_value` is a cross-field rule.
fn sent(body: &UpdateScoreDefinitionRequest, field: &str) -> bool {
    match field {
        "name" => body.name.is_some(),
        "categories" => body.categories.is_some(),
        "min_value" | "max_value" => body.min_value.is_some() || body.max_value.is_some(),
        "description" => body.description.is_some(),
        _ => true,
    }
}

fn to_categories(categories: &[ScoreDefinitionCategory]) -> Vec<DefinitionCategory> {
    categories
        .iter()
        .map(|category| DefinitionCategory {
            label: category.label.clone(),
            value: category.value,
        })
        .collect()
}

fn definition_json(row: &DefinitionRow) -> ScoreDefinition {
    ScoreDefinition {
        id: row.id,
        name: row.name.clone(),
        data_type: row.data_type,
        categories: row
            .categories
            .iter()
            .map(|category| ScoreDefinitionCategory {
                label: category.label.clone(),
                value: category.value,
            })
            .collect(),
        min_value: row.min_value,
        max_value: row.max_value,
        description: row.description.clone(),
        archived_at: row.archived_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// Every field error of a definition of `data_type`. `fields.name` is already trimmed.
/// The rules are those of spec 06 § Score definition contract.
fn check_fields(data_type: ScoreDataType, fields: &DefinitionFields) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if !(1..=255).contains(&fields.name.len()) {
        errors.push(field_error("name", "name must be 1 to 255 bytes"));
    }
    let categorical = data_type == ScoreDataType::Categorical;
    if categorical && !categories_ok(&fields.categories) {
        errors.push(field_error(
            "categories",
            "categories must be 1 to 100 unique labels of 1 to 255 bytes",
        ));
    } else if !categorical && !fields.categories.is_empty() {
        errors.push(field_error(
            "categories",
            "categories are only for categorical scores",
        ));
    }
    let numeric = data_type == ScoreDataType::Numeric;
    if !numeric {
        if fields.min_value.is_some() {
            errors.push(field_error(
                "min_value",
                "ranges are only for numeric scores",
            ));
        }
        if fields.max_value.is_some() {
            errors.push(field_error(
                "max_value",
                "ranges are only for numeric scores",
            ));
        }
    } else if let (Some(min), max) = (fields.min_value, fields.max_value) {
        let ordered = max.is_none_or(|max| min < max);
        if !min.is_finite() || !ordered || max.is_some_and(|max| !max.is_finite()) {
            errors.push(field_error(
                "min_value",
                "min_value must be below max_value",
            ));
        }
    } else if let (None, Some(max)) = (fields.min_value, fields.max_value)
        && !max.is_finite()
    {
        errors.push(field_error(
            "min_value",
            "min_value must be below max_value",
        ));
    }
    if fields
        .description
        .as_deref()
        .is_some_and(|text| text.chars().count() > 1000)
    {
        errors.push(field_error(
            "description",
            "description must be at most 1000 characters",
        ));
    }
    errors
}

/// 1 to 100 categories with unique labels of 1 to 255 bytes and finite values.
fn categories_ok(categories: &[DefinitionCategory]) -> bool {
    if !(1..=100).contains(&categories.len()) {
        return false;
    }
    let mut labels = HashSet::new();
    categories.iter().all(|category| {
        (1..=255).contains(&category.label.len())
            && category.value.is_none_or(f64::is_finite)
            && labels.insert(category.label.as_str())
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::{Method, header};
    use serde_json::{Value, json};

    use super::*;
    use crate::auth::roles::OrgRole;
    use crate::meta::score_definitions::get_definition;
    use crate::test_support::{Member, Seeded, TestApp, TestResponse};

    fn path(project: ProjectId) -> String {
        format!("/api/v1/projects/{project}/score-definitions")
    }

    async fn create(app: &TestApp, seeded: &Seeded, body: Value) -> TestResponse {
        app.request(Method::POST, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(&body)
            .send()
            .await
    }

    async fn create_ok(app: &TestApp, seeded: &Seeded, body: Value) -> Value {
        let response = create(app, seeded, body).await;
        assert_eq!(
            response.status,
            201,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    async fn list(app: &TestApp, seeded: &Seeded, query: &str) -> Value {
        let response = app
            .request(Method::GET, &format!("{}{query}", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    async fn patch(app: &TestApp, seeded: &Seeded, id: &str, body: Value) -> TestResponse {
        app.request(Method::PATCH, &format!("{}/{id}", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(&body)
            .send()
            .await
    }

    async fn delete(app: &TestApp, seeded: &Seeded, id: &str) -> TestResponse {
        app.request(Method::DELETE, &format!("{}/{id}", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await
    }

    /// The `(field, message)` pairs of a 400 answer.
    fn field_errors(response: &TestResponse) -> Vec<(String, String)> {
        assert_eq!(
            response.status,
            400,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body: Value = response.json();
        body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|error| {
                (
                    error["field"].as_str().unwrap().to_owned(),
                    error["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn numeric_body(name: &str) -> Value {
        json!({"name": name, "data_type": "numeric", "min_value": 0.0, "max_value": 1.0})
    }

    fn tone_body() -> Value {
        json!({
            "name": "tone",
            "data_type": "categorical",
            "categories": [{"label": "good", "value": 1.0}, {"label": "bad"}],
            "description": "How the reply sounds"
        })
    }

    #[tokio::test]
    async fn create_list_update_and_archive_round_trip() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(
            &app,
            &seeded,
            json!({
                "name": "  relevance  ",
                "data_type": "numeric",
                "min_value": 0.0,
                "max_value": 1.0,
                "description": "How relevant"
            }),
        )
        .await;
        assert_eq!(created["name"], "relevance");
        assert_eq!(created["data_type"], "numeric");
        assert_eq!(created["categories"], json!([]));
        assert_eq!(created["min_value"], 0.0);
        assert_eq!(created["max_value"], 1.0);
        assert_eq!(created["description"], "How relevant");
        assert!(created.get("archived_at").is_none());
        let id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(created["created_at"], created["updated_at"]);

        let tone = create_ok(&app, &seeded, tone_body()).await;
        assert_eq!(
            tone["categories"][0],
            json!({"label": "good", "value": 1.0})
        );
        assert_eq!(tone["categories"][1], json!({"label": "bad"}));
        assert!(tone.get("min_value").is_none());

        // Listed by name.
        let page = list(&app, &seeded, "").await;
        let names: Vec<&str> = page["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["relevance", "tone"]);
        assert!(page["next_cursor"].is_null());

        // PATCH: rename, clear the description, widen the range.
        let response = patch(
            &app,
            &seeded,
            &id,
            json!({"name": "helpfulness", "description": "", "max_value": 2.0}),
        )
        .await;
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let updated: Value = response.json();
        assert_eq!(updated["name"], "helpfulness");
        assert_eq!(updated["data_type"], "numeric");
        assert_eq!(updated["min_value"], 0.0);
        assert_eq!(updated["max_value"], 2.0);
        assert!(updated.get("description").is_none());

        // DELETE archives: hidden by default, listed with include_archived.
        let response = delete(&app, &seeded, &id).await;
        assert_eq!(response.status, 204);
        let live = list(&app, &seeded, "").await;
        assert_eq!(live["data"].as_array().unwrap().len(), 1);
        let all = list(&app, &seeded, "?include_archived=true").await;
        let archived = all["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id.as_str())
            .unwrap();
        assert!(archived["archived_at"].is_string());
    }

    #[tokio::test]
    async fn names_are_trimmed() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, numeric_body("  padded  ")).await;
        assert_eq!(created["name"], "padded");
        let response = patch(
            &app,
            &seeded,
            created["id"].as_str().unwrap(),
            json!({"name": "\tpadded-2\n"}),
        )
        .await;
        assert_eq!(response.json::<Value>()["name"], "padded-2");
    }

    #[tokio::test]
    async fn validation_reports_every_field() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let long = "d".repeat(1001);
        let response = create(
            &app,
            &seeded,
            json!({
                "name": "   ",
                "data_type": "numeric",
                "categories": [{"label": "a"}],
                "min_value": 1.0,
                "max_value": 0.0,
                "description": long
            }),
        )
        .await;
        let errors = field_errors(&response);
        assert_eq!(
            errors,
            vec![
                ("name".to_owned(), "name must be 1 to 255 bytes".to_owned()),
                (
                    "categories".to_owned(),
                    "categories are only for categorical scores".to_owned()
                ),
                (
                    "min_value".to_owned(),
                    "min_value must be below max_value".to_owned()
                ),
                (
                    "description".to_owned(),
                    "description must be at most 1000 characters".to_owned()
                ),
            ]
        );

        let response = create(
            &app,
            &seeded,
            json!({"name": "tone", "data_type": "categorical", "min_value": 0.0}),
        )
        .await;
        assert_eq!(
            field_errors(&response),
            vec![
                (
                    "categories".to_owned(),
                    "categories must be 1 to 100 unique labels of 1 to 255 bytes".to_owned()
                ),
                (
                    "min_value".to_owned(),
                    "ranges are only for numeric scores".to_owned()
                ),
            ]
        );

        let response = create(
            &app,
            &seeded,
            json!({
                "name": "tone",
                "data_type": "categorical",
                "categories": [{"label": "a"}, {"label": "a"}]
            }),
        )
        .await;
        assert_eq!(field_errors(&response)[0].0, "categories");
    }

    #[tokio::test]
    async fn duplicate_live_name_is_409_and_an_archived_duplicate_is_allowed() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let first = create_ok(&app, &seeded, numeric_body("relevance")).await;
        let duplicate = create(&app, &seeded, numeric_body("relevance")).await;
        assert_eq!(duplicate.status, 409);
        assert!(
            String::from_utf8_lossy(&duplicate.body)
                .contains("a score definition with this name already exists")
        );

        // Archive the first, then a live definition may take its name.
        let first_id = first["id"].as_str().unwrap().to_owned();
        assert_eq!(delete(&app, &seeded, &first_id).await.status, 204);
        let second = create_ok(&app, &seeded, numeric_body("relevance")).await;

        // Restoring the first into the live name is a clash.
        let restored = patch(&app, &seeded, &first_id, json!({"archived": false})).await;
        assert_eq!(restored.status, 409);
        assert!(
            String::from_utf8_lossy(&restored.body)
                .contains("a score definition with this name already exists")
        );
        assert_ne!(second["id"], first["id"]);
    }

    #[tokio::test]
    async fn viewer_lists_and_member_writes() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let member: Member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        create_ok(&app, &seeded, numeric_body("relevance")).await;

        let list_as_viewer = app
            .request(Method::GET, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &viewer.cookie)
            .send()
            .await;
        assert_eq!(list_as_viewer.status, 200);
        let write_as_viewer = app
            .request(Method::POST, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &viewer.cookie)
            .json(&numeric_body("tone-2"))
            .send()
            .await;
        assert_eq!(write_as_viewer.status, 403);

        let write_as_member = app
            .request(Method::POST, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &member.cookie)
            .json(&numeric_body("tone-2"))
            .send()
            .await;
        assert_eq!(write_as_member.status, 201);
    }

    #[tokio::test]
    async fn api_keys_follow_their_scopes() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let read = app
            .request(Method::GET, &path(seeded.project.id))
            .header(
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {}", seeded.read_key),
            )
            .send()
            .await;
        assert_eq!(read.status, 200);

        let minted = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/api-keys", seeded.project.id),
            )
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(&json!({"kind": "secret", "scopes": ["ingest", "read", "write"]}))
            .send()
            .await;
        assert_eq!(
            minted.status,
            201,
            "{}",
            String::from_utf8_lossy(&minted.body)
        );
        let secret = minted.json::<Value>()["secret"]
            .as_str()
            .unwrap()
            .to_owned();
        let write = app
            .request(Method::POST, &path(seeded.project.id))
            .header(header::AUTHORIZATION.as_str(), &format!("Bearer {secret}"))
            .json(&numeric_body("relevance"))
            .send()
            .await;
        assert_eq!(write.status, 403);
    }

    #[tokio::test]
    async fn patch_validates_against_the_stored_type() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let numeric = create_ok(&app, &seeded, numeric_body("relevance")).await;
        let response = patch(
            &app,
            &seeded,
            numeric["id"].as_str().unwrap(),
            json!({"categories": [{"label": "a"}]}),
        )
        .await;
        assert_eq!(field_errors(&response)[0].0, "categories");

        let tone = create_ok(&app, &seeded, tone_body()).await;
        let response = patch(
            &app,
            &seeded,
            tone["id"].as_str().unwrap(),
            json!({"min_value": 0.0}),
        )
        .await;
        assert_eq!(
            field_errors(&response),
            vec![(
                "min_value".to_owned(),
                "ranges are only for numeric scores".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn empty_patch_only_bumps_updated_at() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, numeric_body("relevance")).await;
        app.state.clock.advance(Duration::from_secs(1));
        let response = patch(&app, &seeded, created["id"].as_str().unwrap(), json!({})).await;
        assert_eq!(response.status, 200);
        let updated: Value = response.json();
        assert_eq!(updated["created_at"], created["created_at"]);
        assert_ne!(updated["updated_at"], created["updated_at"]);
        let mut without_times = updated;
        without_times["updated_at"] = created["updated_at"].clone();
        assert_eq!(without_times, created);
    }

    #[tokio::test]
    async fn list_pages_by_name() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        for name in ["c", "a", "b"] {
            create_ok(&app, &seeded, numeric_body(name)).await;
        }
        let mut names = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let query = match &cursor {
                None => "?limit=1".to_owned(),
                Some(next) => format!("?limit=1&cursor={next}"),
            };
            let page = list(&app, &seeded, &query).await;
            names.extend(
                page["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["name"].as_str().unwrap().to_owned()),
            );
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn another_projects_definition_is_404() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (other, _key) = app.add_project(&seeded, "other", None).await;
        let created = app
            .request(Method::POST, &path(other.id))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(&numeric_body("relevance"))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let id = created.json::<Value>()["id"].as_str().unwrap().to_owned();

        let response = patch(&app, &seeded, &id, json!({"description": "x"})).await;
        assert_eq!(response.status, 404);
        assert_eq!(delete(&app, &seeded, &id).await.status, 404);
    }

    #[tokio::test]
    async fn a_stale_patch_is_409() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, numeric_body("relevance")).await;
        let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        let project = seeded.project.id;
        let stale = get_definition(app.state.meta.reader(), project, id)
            .await
            .unwrap()
            .unwrap();

        // Another writer changes the row after the handler read it.
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE score_definitions SET description = 'x', updated_at = updated_at + 1 \
                         WHERE id = ?",
                    )
                    .bind(id.to_string())
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();

        let fields = DefinitionFields {
            name: "relevance-2".to_owned(),
            ..fields_of(&stale)
        };
        let now = app.state.clock.now_ns();
        let error = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move { write_update(conn, project, id, &stale, fields, now).await })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("score definition changed, retry")
            ),
            "{error}"
        );
    }
}
