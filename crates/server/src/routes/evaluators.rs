//! Evaluator routes and built-in templates (spec 09 § Evaluation & alerts, spec 06
//! § Evaluators, D-I-19, D-I-20, D-I-24). Members and above write; every project reader
//! lists and reads.

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use llmobs_core::ids::{OrgId, ProjectId};
use llmobs_eval::{EvaluatorConfig, JudgeOutput, OutputScore};
use serde::Deserialize;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::api::common::{FieldError, Page, PageParams};
use crate::api::evaluators::{
    CreateEvaluatorRequest, Evaluator, EvaluatorKind, EvaluatorTemplateList, EvaluatorTestRequest,
    EvaluatorTestResult, OutputScoreBody, UpdateEvaluatorRequest, builtin_template_list,
};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::ProjectPerm;
use crate::crypto::{Aad, SecretBox};
use crate::eval::definitions::fits_definition;
use crate::eval::evaluator::{self, EvaluatorError, Prepared, WebhookSecrets};
use crate::eval::test_run;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::MetaError;
use crate::meta::connections;
use crate::meta::evaluators::{self as store, EvaluatorFields, EvaluatorRow};
use crate::meta::score_definitions::list_definitions;

use super::orgs::page_named;

/// Evaluator routes. Reads need project view; writes need
/// [`ProjectPerm::ManageEvaluation`] (member and above, D-I-24). Not session-only.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}/evaluators",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_evaluators),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/projects/{project}/evaluators",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: post(create_evaluator),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/projects/{project}/evaluators/test",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: post(test_evaluator),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}/evaluators/{id}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_evaluator),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/projects/{project}/evaluators/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: patch(update_evaluator),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/projects/{project}/evaluators/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageEvaluation,
                session_only: false,
            },
            documented: true,
            handler: delete(delete_evaluator),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}/evaluator-templates",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_evaluator_templates),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct IdPath {
    id: Uuid,
}

/// What a write transaction did.
enum Outcome {
    /// The row as stored.
    Updated(Box<EvaluatorRow>),
    /// The evaluator is missing.
    Gone,
    /// A judge's connection is not in the org.
    ConnectionMissing,
}

async fn list_evaluators(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<Evaluator>>, ApiError> {
    let rows = store::list_evaluators(state.meta.reader(), access.project.id).await?;
    let items = rows
        .iter()
        .map(|row| Ok((row.name.clone(), row.id.to_string(), evaluator_json(row)?)))
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(error::Json(page_named(
        items,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn get_evaluator(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
) -> Result<error::Json<Evaluator>, ApiError> {
    let row = store::get_evaluator(state.meta.reader(), access.project.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(error::Json(evaluator_json(&row)?))
}

async fn create_evaluator(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateEvaluatorRequest>,
) -> Result<(StatusCode, error::Json<Evaluator>), ApiError> {
    let user = principal::session_user(&principal)?;
    let id = Uuid::now_v7();
    let project = access.project.id;
    let org = access.project.org_id;
    let kind = body.kind;
    let output = output_of(&body.output_score);
    let prepared = evaluator::prepare(
        id,
        &body.name,
        kind,
        &body.config,
        &output,
        body.webhook_secret.as_deref(),
        "",
    )
    .map_err(bad_request)?;
    check_against_store(state.meta.reader(), org, project, &prepared).await?;
    let host = prepared
        .secrets
        .as_ref()
        .and_then(|values| evaluator::url_host(&values.url));
    let secret_enc = prepared
        .secrets
        .as_ref()
        .map(|values| seal_secrets(&state.secrets, id, values))
        .transpose()?;
    let fields = EvaluatorFields {
        name: prepared.evaluator.name.clone(),
        config: prepared.config_json,
        output_score: prepared.output_json,
        connection_id: prepared.connection_id,
        secret_enc,
        version: 1,
    };
    let user_id = user.id;
    let connection_id = fields.connection_id;
    let now = state.clock.now_ns();
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                // The connection is read again inside the insert's transaction.
                if let Some(connection) = connection_id
                    && connections::get_connection_tx(conn, org, connection)
                        .await?
                        .is_none()
                {
                    return Ok(Outcome::ConnectionMissing);
                }
                let row = store::insert_evaluator(
                    conn,
                    id,
                    project,
                    kind.as_str(),
                    fields,
                    Some(user_id),
                    now,
                )
                .await?;
                Ok(Outcome::Updated(Box::new(row)))
            })
        })
        .await?;
    match outcome {
        Outcome::Updated(row) => {
            tracing::info!(
                target: "audit",
                evaluator_id = %id,
                project_id = %project,
                user_id = %user_id,
                kind = kind.as_str(),
                url_host = host.as_deref().unwrap_or(""),
                "evaluator created"
            );
            Ok((StatusCode::CREATED, error::Json(evaluator_json(&row)?)))
        }
        Outcome::ConnectionMissing => Err(bad_request(vec![connection_missing()])),
        Outcome::Gone => Err(ApiError::NotFound),
    }
}

async fn update_evaluator(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
    error::Json(body): error::Json<UpdateEvaluatorRequest>,
) -> Result<error::Json<Evaluator>, ApiError> {
    let user = principal::session_user(&principal)?;
    let project = access.project.id;
    let org = access.project.org_id;
    let expected = store::get_evaluator(state.meta.reader(), project, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let kind = EvaluatorKind::from_name(&expected.kind).ok_or_else(|| corrupt("kind"))?;
    let (current, opened) = evaluator::load(&state.secrets, &expected).map_err(load_error)?;
    // Merge the PATCH over the stored values. The sealed values are opened above, so
    // sealing (which needs the key) happens outside the transaction.
    let config_sent = body.config.is_some();
    let secret_sent = body.webhook_secret.is_some();
    let name = body.name.unwrap_or_else(|| expected.name.clone());
    let output = body
        .output_score
        .as_ref()
        .map_or_else(|| current.output.clone(), output_of);
    let config = match body.config {
        Some(config) => config,
        None => evaluator::config_document(&current, opened.as_ref()).map_err(load_error)?,
    };
    let secret = body
        .webhook_secret
        .or_else(|| opened.as_ref().map(|values| values.signing_secret.clone()));
    let prepared = evaluator::prepare(id, &name, kind, &config, &output, secret.as_deref(), "")
        .map_err(bad_request)?;
    check_against_store(state.meta.reader(), org, project, &prepared).await?;
    let host = prepared
        .secrets
        .as_ref()
        .and_then(|values| evaluator::url_host(&values.url));
    // The sealed values stay as they are unless the PATCH sent webhook values.
    let secret_enc = match prepared.secrets.as_ref() {
        None => None,
        Some(_) if !config_sent && !secret_sent => expected.secret_enc.clone(),
        Some(values) => Some(seal_secrets(&state.secrets, id, values)?),
    };
    // The version moves only when the stored config, output or sealed values change.
    let before = opened.as_ref().map(secrets_text).transpose()?;
    let after = prepared.secrets.as_ref().map(secrets_text).transpose()?;
    let changed = prepared.config_json != expected.config
        || prepared.output_json != expected.output_score
        || before != after;
    let version = if changed {
        expected.version.saturating_add(1)
    } else {
        expected.version
    };
    let fields = EvaluatorFields {
        name: prepared.evaluator.name.clone(),
        config: prepared.config_json,
        output_score: prepared.output_json,
        connection_id: prepared.connection_id,
        secret_enc,
        version,
    };
    let user_id = user.id;
    let now = state.clock.now_ns();
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(
                async move { write_update(conn, org, project, id, expected, fields, now).await },
            )
        })
        .await?;
    match outcome {
        Outcome::Updated(row) => {
            tracing::info!(
                target: "audit",
                evaluator_id = %id,
                project_id = %project,
                user_id = %user_id,
                url_host = host.as_deref().unwrap_or(""),
                "evaluator updated"
            );
            Ok(error::Json(evaluator_json(&row)?))
        }
        Outcome::Gone => Err(ApiError::NotFound),
        Outcome::ConnectionMissing => Err(bad_request(vec![connection_missing()])),
    }
}

async fn delete_evaluator(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
) -> Result<StatusCode, ApiError> {
    let user = principal::session_user(&principal)?;
    let project = access.project.id;
    let user_id = user.id;
    let went = state
        .meta
        .write(move |conn| {
            Box::pin(async move { store::delete_evaluator(conn, project, id).await })
        })
        .await?;
    if !went {
        return Err(ApiError::NotFound);
    }
    tracing::info!(
        target: "audit",
        evaluator_id = %id,
        project_id = %project,
        user_id = %user_id,
        "evaluator deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Runs one evaluator once and answers the outcome. It writes no score (D-I-21).
async fn test_evaluator(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<EvaluatorTestRequest>,
) -> Result<error::Json<EvaluatorTestResult>, ApiError> {
    let user = principal::session_user(&principal)?;
    let result = test_run::run(&state, &access.project, user.id, body).await?;
    Ok(error::Json(result))
}

async fn list_evaluator_templates() -> error::Json<EvaluatorTemplateList> {
    error::Json(builtin_template_list())
}

/// Step 7 of the update: refuses a row that changed since `expected` was read, then
/// writes `fields`. A judge's connection is read in the same transaction.
async fn write_update(
    conn: &mut SqliteConnection,
    org: OrgId,
    project: ProjectId,
    id: Uuid,
    expected: EvaluatorRow,
    fields: EvaluatorFields,
    now: i64,
) -> Result<Outcome, MetaError> {
    let Some(current) = store::get_evaluator_tx(conn, project, id).await? else {
        return Ok(Outcome::Gone);
    };
    if current != expected {
        return Err(MetaError::Conflict("evaluator changed, retry"));
    }
    if let Some(connection) = fields.connection_id
        && connections::get_connection_tx(conn, org, connection)
            .await?
            .is_none()
    {
        return Ok(Outcome::ConnectionMissing);
    }
    Ok(
        match store::update_evaluator(conn, project, id, fields, now).await? {
            Some(row) => Outcome::Updated(Box::new(row)),
            None => Outcome::Gone,
        },
    )
}

/// The checks that need `meta.db`: a judge's connection is in the org, and the output fits
/// the live score definition of its name (spec 06 § Evaluator contract).
async fn check_against_store(
    reader: &SqlitePool,
    org: OrgId,
    project: ProjectId,
    prepared: &Prepared,
) -> Result<(), ApiError> {
    let mut errors = Vec::new();
    if let Some(connection) = prepared.connection_id
        && connections::get_connection(reader, org, connection)
            .await?
            .is_none()
    {
        errors.push(connection_missing());
    }
    let definitions = list_definitions(reader, project, false).await?;
    let judge: Option<&JudgeOutput> = match &prepared.evaluator.config {
        EvaluatorConfig::LlmJudge(judge) => Some(&judge.output),
        EvaluatorConfig::Check(_) | EvaluatorConfig::Webhook(_) => None,
    };
    if let Some(definition) = definitions
        .iter()
        .find(|definition| definition.name == prepared.evaluator.output.name)
        && !fits_definition(&prepared.evaluator.output, judge, definition)
    {
        errors.push(field_error(
            "output_score",
            "output_score does not match the score definition",
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(bad_request(errors))
    }
}

/// The evaluator as the API shows it: the stored JSON, without the sealed values.
fn evaluator_json(row: &EvaluatorRow) -> Result<Evaluator, ApiError> {
    Ok(Evaluator {
        id: row.id,
        name: row.name.clone(),
        kind: EvaluatorKind::from_name(&row.kind).ok_or_else(|| corrupt("kind"))?,
        config: serde_json::from_str(&row.config).map_err(|_| corrupt("config"))?,
        output_score: serde_json::from_str::<OutputScore>(&row.output_score)
            .map(|output| body_of(&output))
            .map_err(|_| corrupt("output_score"))?,
        has_signing_secret: row.secret_enc.is_some(),
        version: i64::from(row.version),
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn output_of(body: &OutputScoreBody) -> OutputScore {
    OutputScore {
        name: body.name.clone(),
        data_type: body.data_type,
        categories: body.categories.clone(),
    }
}

fn body_of(output: &OutputScore) -> OutputScoreBody {
    OutputScoreBody {
        name: output.name.clone(),
        data_type: output.data_type,
        categories: output.categories.clone(),
    }
}

/// Seals the webhook values for the row `id`.
fn seal_secrets(
    secrets: &SecretBox,
    id: Uuid,
    values: &WebhookSecrets,
) -> Result<Vec<u8>, ApiError> {
    let bytes = serde_json::to_vec(values).map_err(internal)?;
    secrets
        .seal(
            &bytes,
            Aad {
                table: "evaluators",
                column: "secret_enc",
                row_id: &id,
            },
        )
        .map_err(internal)
}

/// The webhook values as the JSON text compared for the version rule.
fn secrets_text(values: &WebhookSecrets) -> Result<String, ApiError> {
    serde_json::to_string(values).map_err(internal)
}

fn connection_missing() -> FieldError {
    field_error(
        "config.connection_id",
        "connection not found in this organization",
    )
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

fn corrupt(column: &'static str) -> ApiError {
    ApiError::Internal(Box::new(MetaError::Corrupt {
        table: "evaluators",
        column,
    }))
}

fn load_error(error: EvaluatorError) -> ApiError {
    ApiError::Internal(Box::new(error))
}

fn internal(error: impl std::error::Error + Send + Sync + 'static) -> ApiError {
    ApiError::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use axum::http::{Method, header};
    use serde_json::{Value, json};

    use super::*;
    use crate::auth::roles::OrgRole;
    use crate::meta::connections::{ConnectionFields, insert_connection};
    use crate::meta::orgs::insert_org;
    use crate::test_support::mock_llm::{MockLlm, Reply, completion};
    use crate::test_support::mock_webhook::MockWebhook;
    use crate::test_support::{Seeded, TestApp, TestResponse, capture_logs, dataset_span};
    use llmobs_core::enums::PayloadFormat;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::span::Span;

    /// The fixture judge's connection (`fixtures/api/README.md`).
    const FIXTURE_CONNECTION: &str = "01900000-0000-7000-8000-000000000400";
    const WEBHOOK_SECRET: &str = "signing-secret-0123456789";
    const WEBHOOK_URL: &str = "https://hooks.example.com/private/path?token=query-secret";
    const WEBHOOK_HEADER_VALUE: &str = "header-secret-value";

    fn path(project: ProjectId) -> String {
        format!("/api/v1/projects/{project}/evaluators")
    }

    /// The fixture create body (`fixtures/api/requests/evaluator-create.json`).
    fn judge_body() -> Value {
        serde_json::from_str(include_str!(
            "../../../../fixtures/api/requests/evaluator-create.json"
        ))
        .unwrap()
    }

    fn check_body(name: &str) -> Value {
        json!({
            "name": name,
            "kind": "check",
            "config": {"check": "contains", "value": "refund"},
            "output_score": {"name": "mentions_refund", "data_type": "boolean"}
        })
    }

    fn webhook_body(name: &str) -> Value {
        json!({
            "name": name,
            "kind": "webhook",
            "config": {
                "url": WEBHOOK_URL,
                "headers": {"X-Team": WEBHOOK_HEADER_VALUE}
            },
            "output_score": {"name": "hook", "data_type": "numeric"},
            "webhook_secret": WEBHOOK_SECRET
        })
    }

    /// Inserts a connection with `id` in `org`, as M1's route would, and returns its id.
    async fn add_connection(app: &TestApp, org: OrgId, id: Uuid) -> Uuid {
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_connection(
                        conn,
                        id,
                        org,
                        ConnectionFields {
                            name: format!("Judge {id}"),
                            adapter: "openai".to_owned(),
                            base_url: None,
                            secret_enc: vec![0x01],
                            api_key_hint: "0123".to_owned(),
                            extra_headers_enc: None,
                            extra_header_names: Vec::new(),
                            default_model: None,
                            structured_output: "native".to_owned(),
                            max_concurrency: 4,
                            requests_per_minute: 300,
                        },
                        None,
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        id
    }

    /// The fixture connection, in `seeded`'s org.
    async fn fixture_connection(app: &TestApp, seeded: &Seeded) {
        add_connection(
            app,
            seeded.org_id,
            Uuid::from_str(FIXTURE_CONNECTION).unwrap(),
        )
        .await;
    }

    async fn create(app: &TestApp, seeded: &Seeded, body: &Value) -> TestResponse {
        app.request(Method::POST, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(body)
            .send()
            .await
    }

    async fn create_ok(app: &TestApp, seeded: &Seeded, body: &Value) -> Value {
        let response = create(app, seeded, body).await;
        assert_eq!(response.status, 201, "{}", text(&response));
        response.json()
    }

    async fn patch(app: &TestApp, seeded: &Seeded, id: &str, body: &Value) -> TestResponse {
        app.request(Method::PATCH, &format!("{}/{id}", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(body)
            .send()
            .await
    }

    async fn patch_ok(app: &TestApp, seeded: &Seeded, id: &str, body: &Value) -> Value {
        let response = patch(app, seeded, id, body).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        response.json()
    }

    fn text(response: &TestResponse) -> String {
        String::from_utf8_lossy(&response.body).into_owned()
    }

    /// The `(field, message)` pairs of a 400 answer.
    fn field_errors(response: &TestResponse) -> Vec<(String, String)> {
        assert_eq!(response.status, 400, "{}", text(response));
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

    /// The stored `secret_enc` of `id`.
    async fn stored_secret(app: &TestApp, id: Uuid) -> Vec<u8> {
        sqlx::query_scalar::<_, Option<Vec<u8>>>("SELECT secret_enc FROM evaluators WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap()
            .expect("a sealed webhook value")
    }

    /// The evaluator without the fields that differ between runs.
    fn without_identity(value: &Value) -> Value {
        let mut value = value.clone();
        let object = value.as_object_mut().unwrap();
        for key in ["id", "created_by", "created_at", "updated_at"] {
            object.remove(key);
        }
        value
    }

    fn id_of(value: &Value) -> String {
        value["id"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn judge_round_trip() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let created = create_ok(&app, &seeded, &judge_body()).await;
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/static/evaluator.json"
        ))
        .unwrap();
        // The fixture's author is a fixed id; the seeded owner is not, so it is not compared.
        assert_eq!(without_identity(&created), without_identity(&expected));
        assert!(created["created_by"].is_string());
        let id = id_of(&created);
        let read = app
            .request(Method::GET, &format!("{}/{id}", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(read.status, 200);
        assert_eq!(read.json::<Value>(), created);
        let page = app
            .request(Method::GET, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await
            .json::<Value>();
        assert_eq!(page["data"], json!([created]));
    }

    #[tokio::test]
    async fn check_round_trip() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, &check_body("Mentions refund")).await;
        assert_eq!(created["kind"], "check");
        assert_eq!(created["config"]["check"], "contains");
        // The folded needle is stored in `values`; `value` stays as null.
        assert_eq!(created["config"]["values"], json!(["refund"]));
        assert!(created["config"]["value"].is_null());
        assert_eq!(
            created["output_score"],
            json!({"name": "mentions_refund", "data_type": "boolean"})
        );
        assert_eq!(created["has_signing_secret"], false);
    }

    #[tokio::test]
    async fn webhook_values_are_sealed() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, &webhook_body("Hooked")).await;
        assert_eq!(
            created["config"],
            json!({
                "url_host": "hooks.example.com",
                "timeout_ms": 10_000,
                "header_names": ["x-team"]
            })
        );
        assert_eq!(created["has_signing_secret"], true);
        let response = created.to_string();
        for sealed in [
            WEBHOOK_SECRET,
            "/private/path",
            "query-secret",
            WEBHOOK_HEADER_VALUE,
        ] {
            assert!(!response.contains(sealed), "response shows {sealed}");
        }
        // The stored values open with their own row id, and with no other.
        let id = Uuid::from_str(&id_of(&created)).unwrap();
        let sealed = stored_secret(&app, id).await;
        let aad = Aad {
            table: "evaluators",
            column: "secret_enc",
            row_id: &id,
        };
        let opened: Value =
            serde_json::from_slice(&app.state.secrets.open(&sealed, aad).unwrap()).unwrap();
        assert_eq!(opened["signing_secret"], WEBHOOK_SECRET);
        assert_eq!(opened["url"], WEBHOOK_URL);
        assert_eq!(opened["headers"]["X-Team"], WEBHOOK_HEADER_VALUE);
        let other = Uuid::now_v7();
        let wrong = Aad {
            table: "evaluators",
            column: "secret_enc",
            row_id: &other,
        };
        assert!(app.state.secrets.open(&sealed, wrong).is_err());
    }

    #[tokio::test]
    async fn webhook_needs_a_secret() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut body = webhook_body("Hooked");
        body.as_object_mut().unwrap().remove("webhook_secret");
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [(
                "webhook_secret".to_owned(),
                "webhook_secret is required".to_owned()
            )]
        );
        body["webhook_secret"] = json!("short");
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [(
                "webhook_secret".to_owned(),
                "webhook_secret must be 16 to 256 characters".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn webhook_secret_only_for_webhooks() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let mut body = judge_body();
        body["webhook_secret"] = json!(WEBHOOK_SECRET);
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [(
                "webhook_secret".to_owned(),
                "webhook_secret is only for webhook evaluators".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn validation_maps_config_errors() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;

        let mut unknown_check = check_body("Bad");
        unknown_check["config"] = json!({"check": "nope"});
        let errors = field_errors(&create(&app, &seeded, &unknown_check).await);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "config");
        assert!(errors[0].1.starts_with("invalid configuration: "));

        let mut no_prompt = judge_body();
        no_prompt["config"]["prompt"] = json!("");
        assert_eq!(
            field_errors(&create(&app, &seeded, &no_prompt).await),
            [("config.prompt".to_owned(), "must be non-empty".to_owned())]
        );

        let mut mismatch = judge_body();
        mismatch["output_score"]["data_type"] = json!("boolean");
        assert_eq!(
            field_errors(&create(&app, &seeded, &mismatch).await),
            [(
                "output_score.data_type".to_owned(),
                "data_type must be numeric for this evaluator".to_owned()
            )]
        );

        let mut no_name = judge_body();
        no_name["name"] = json!("   ");
        assert_eq!(
            field_errors(&create(&app, &seeded, &no_name).await),
            [("name".to_owned(), "must be 1 to 128 characters".to_owned())]
        );
    }

    #[tokio::test]
    async fn connection_must_be_in_the_org() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let other = app
            .state
            .meta
            .write(move |conn| Box::pin(async move { insert_org(conn, "Other", "other", 1).await }))
            .await
            .unwrap();
        let foreign = add_connection(&app, other.id, Uuid::now_v7()).await;
        let mut body = judge_body();
        body["config"]["connection_id"] = json!(foreign);
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [(
                "config.connection_id".to_owned(),
                "connection not found in this organization".to_owned()
            )]
        );
        // A connection that does not exist at all is refused the same way.
        body["config"]["connection_id"] = json!(Uuid::now_v7());
        assert_eq!(field_errors(&create(&app, &seeded, &body).await).len(), 1);
    }

    #[tokio::test]
    async fn output_must_fit_the_definition() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let definition = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/score-definitions", seeded.project.id),
            )
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(&json!({
                "name": "relevance",
                "data_type": "numeric",
                "min_value": 0.0,
                "max_value": 1.0
            }))
            .send()
            .await;
        assert_eq!(definition.status, 201);

        // The judge's range 0 to 2 is wider than the definition's 0 to 1.
        let mut wide = judge_body();
        wide["config"]["output"]["max"] = json!(2.0);
        assert_eq!(
            field_errors(&create(&app, &seeded, &wide).await),
            [(
                "output_score".to_owned(),
                "output_score does not match the score definition".to_owned()
            )]
        );
        create_ok(&app, &seeded, &judge_body()).await;
    }

    #[tokio::test]
    async fn version_bumps_only_on_content() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let created = create_ok(&app, &seeded, &judge_body()).await;
        let id = id_of(&created);
        assert_eq!(created["version"], 1);

        let renamed = patch_ok(&app, &seeded, &id, &json!({"name": "Relevance v2"})).await;
        assert_eq!(renamed["version"], 1);
        assert_eq!(renamed["name"], "Relevance v2");

        let mut config = judge_body()["config"].clone();
        config["prompt"] = json!("Rate the answer. {{ input }} {{ output }}");
        let reworded = patch_ok(&app, &seeded, &id, &json!({"config": config})).await;
        assert_eq!(reworded["version"], 2);

        let webhook = create_ok(&app, &seeded, &webhook_body("Hooked")).await;
        let hook_id = id_of(&webhook);
        let resealed = patch_ok(
            &app,
            &seeded,
            &hook_id,
            &json!({"webhook_secret": "another-signing-secret-99"}),
        )
        .await;
        assert_eq!(resealed["version"], 2);
        // The same secret again changes nothing.
        let same = patch_ok(
            &app,
            &seeded,
            &hook_id,
            &json!({"webhook_secret": "another-signing-secret-99"}),
        )
        .await;
        assert_eq!(same["version"], 2);
    }

    #[tokio::test]
    async fn kind_cannot_change() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let created = create_ok(&app, &seeded, &judge_body()).await;
        let response = patch(&app, &seeded, &id_of(&created), &json!({"kind": "check"})).await;
        assert_eq!(response.status, 400, "{}", text(&response));
    }

    #[tokio::test]
    async fn patch_webhook_without_config_keeps_the_sealed_values() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, &webhook_body("Hooked")).await;
        let id = id_of(&created);
        let uuid = Uuid::from_str(&id).unwrap();
        let before = stored_secret(&app, uuid).await;

        patch_ok(&app, &seeded, &id, &json!({"name": "Renamed"})).await;
        assert_eq!(stored_secret(&app, uuid).await, before);

        let moved = patch_ok(
            &app,
            &seeded,
            &id,
            &json!({"config": {"url": "https://hooks.example.net/new", "headers": {"X-Team": "new-value"}}}),
        )
        .await;
        assert_eq!(moved["version"], 2);
        assert_eq!(moved["config"]["url_host"], "hooks.example.net");
        assert_eq!(moved["config"]["header_names"], json!(["x-team"]));
        let aad = Aad {
            table: "evaluators",
            column: "secret_enc",
            row_id: &uuid,
        };
        let opened: Value = serde_json::from_slice(
            &app.state
                .secrets
                .open(&stored_secret(&app, uuid).await, aad)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(opened["signing_secret"], WEBHOOK_SECRET);
        assert_eq!(opened["url"], "https://hooks.example.net/new");
        assert_eq!(opened["headers"]["X-Team"], "new-value");
    }

    #[tokio::test]
    async fn duplicate_name_is_409() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        create_ok(&app, &seeded, &judge_body()).await;
        let response = create(&app, &seeded, &judge_body()).await;
        assert_eq!(response.status, 409, "{}", text(&response));
        assert!(text(&response).contains("an evaluator with this name already exists"));
    }

    #[tokio::test]
    async fn member_writes_viewer_reads() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let response = app
            .request(Method::POST, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &member.cookie)
            .json(&judge_body())
            .send()
            .await;
        assert_eq!(response.status, 201, "{}", text(&response));
        let listed = app
            .request(Method::GET, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &viewer.cookie)
            .send()
            .await;
        assert_eq!(listed.status, 200);
        let refused = app
            .request(Method::POST, &path(seeded.project.id))
            .header(header::COOKIE.as_str(), &viewer.cookie)
            .json(&check_body("Refused"))
            .send()
            .await;
        assert_eq!(refused.status, 403);
    }

    #[tokio::test]
    async fn api_key_cannot_write() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let listed = app
            .request(Method::GET, &path(seeded.project.id))
            .header(
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {}", seeded.read_key),
            )
            .send()
            .await;
        assert_eq!(listed.status, 200);
        let refused = app
            .request(Method::POST, &path(seeded.project.id))
            .header(
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {}", seeded.read_key),
            )
            .json(&check_body("Refused"))
            .send()
            .await;
        assert_eq!(refused.status, 403);
    }

    #[tokio::test]
    async fn templates_match_the_fixture() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/evaluator-templates", seeded.project.id),
            )
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(response.status, 200);
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/static/evaluator-templates.json"
        ))
        .unwrap();
        assert_eq!(response.json::<Value>(), expected);
    }

    #[tokio::test]
    async fn connection_delete_is_409_while_used() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let created = create_ok(&app, &seeded, &judge_body()).await;
        let connection = format!("/api/v1/orgs/acme/llm-connections/{FIXTURE_CONNECTION}");
        let blocked = app
            .request(Method::DELETE, &connection)
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(blocked.status, 409, "{}", text(&blocked));
        assert!(text(&blocked).contains("connection is used by evaluators"));

        let removed = app
            .request(
                Method::DELETE,
                &format!("{}/{}", path(seeded.project.id), id_of(&created)),
            )
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(removed.status, 204);
        let freed = app
            .request(Method::DELETE, &connection)
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(freed.status, 204);
    }

    #[tokio::test]
    async fn another_projects_evaluator_is_404() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (other, _) = app.add_project(&seeded, "other", None).await;
        let created = create_ok(&app, &seeded, &check_body("Mine")).await;
        let id = id_of(&created);
        let read = app
            .request(Method::GET, &format!("{}/{id}", path(other.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(read.status, 404);
        let edited = app
            .request(Method::PATCH, &format!("{}/{id}", path(other.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .json(&json!({"name": "x"}))
            .send()
            .await;
        assert_eq!(edited.status, 404);
        let removed = app
            .request(Method::DELETE, &format!("{}/{id}", path(other.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(removed.status, 404);
    }

    #[tokio::test]
    async fn patch_validation_maps_fields() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let created = create_ok(&app, &seeded, &judge_body()).await;
        let id = id_of(&created);

        let mut config = judge_body()["config"].clone();
        config["prompt"] = json!("");
        assert_eq!(
            field_errors(&patch(&app, &seeded, &id, &json!({"config": config})).await),
            [("config.prompt".to_owned(), "must be non-empty".to_owned())]
        );
        assert_eq!(
            field_errors(
                &patch(
                    &app,
                    &seeded,
                    &id,
                    &json!({"output_score": {"name": "", "data_type": "numeric"}})
                )
                .await
            ),
            [(
                "output_score.name".to_owned(),
                "output_score.name must be 1 to 255 bytes".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn a_stale_patch_is_409() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        fixture_connection(&app, &seeded).await;
        let created = create_ok(&app, &seeded, &judge_body()).await;
        let id = Uuid::from_str(&id_of(&created)).unwrap();
        let project = seeded.project.id;
        let org = seeded.org_id;
        let now = app.state.clock.now_ns();

        let expected = store::get_evaluator(app.state.meta.reader(), project, id)
            .await
            .unwrap()
            .unwrap();
        // Another write lands between the PATCH's read and its write.
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE evaluators SET updated_at = updated_at + 1 WHERE id = ?")
                        .bind(id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok::<(), MetaError>(())
                })
            })
            .await
            .unwrap();
        let fields = EvaluatorFields {
            name: "Stale".to_owned(),
            config: expected.config.clone(),
            output_score: expected.output_score.clone(),
            connection_id: expected.connection_id,
            secret_enc: None,
            version: 2,
        };
        let stale = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    write_update(conn, org, project, id, expected, fields, now).await
                })
            })
            .await;
        assert!(matches!(
            stale,
            Err(MetaError::Conflict("evaluator changed, retry"))
        ));

        // A judge whose connection row is gone is reported, not written.
        let current = store::get_evaluator(app.state.meta.reader(), project, id)
            .await
            .unwrap()
            .unwrap();
        let fields = EvaluatorFields {
            name: current.name.clone(),
            config: current.config.clone(),
            output_score: current.output_score.clone(),
            connection_id: Some(Uuid::now_v7()),
            secret_enc: None,
            version: current.version,
        };
        let outcome = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(
                    async move { write_update(conn, org, project, id, current, fields, now).await },
                )
            })
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::ConnectionMissing));
    }

    #[tokio::test]
    async fn webhook_header_values_are_checked() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut body = webhook_body("Hooked");
        body["config"]["headers"] = json!({"X-A": "one", "x-a": "two"});
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [("config.headers.x-a".to_owned(), "invalid header".to_owned())]
        );
        body["config"]["headers"] = json!({"X-Bad": "line\r\nbreak"});
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [(
                "config.headers.x-bad".to_owned(),
                "invalid header".to_owned()
            )]
        );
        body["config"]["headers"] = json!({"X-Long": "a".repeat(4097)});
        assert_eq!(
            field_errors(&create(&app, &seeded, &body).await),
            [(
                "config.headers.x-long".to_owned(),
                "invalid header".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn evaluator_cap_is_500() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    for index in 0..500 {
                        let fields = EvaluatorFields {
                            name: format!("row-{index:03}"),
                            config: "{}".to_owned(),
                            output_score: "{}".to_owned(),
                            connection_id: None,
                            secret_enc: None,
                            version: 1,
                        };
                        store::insert_evaluator(
                            conn,
                            Uuid::now_v7(),
                            project,
                            "check",
                            fields,
                            None,
                            now,
                        )
                        .await?;
                    }
                    Ok::<(), MetaError>(())
                })
            })
            .await
            .unwrap();
        let response = create(&app, &seeded, &check_body("Five hundred and first")).await;
        assert_eq!(response.status, 409, "{}", text(&response));
        assert!(text(&response).contains("a project has at most 500 evaluators"));
    }

    #[tokio::test]
    async fn list_pages_by_name() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        for name in ["c", "a", "b"] {
            create_ok(&app, &seeded, &check_body(name)).await;
        }
        let first = app
            .request(Method::GET, &format!("{}?limit=2", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await
            .json::<Value>();
        let names = |page: &Value| -> Vec<String> {
            page["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["name"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(names(&first), ["a", "b"]);
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = app
            .request(
                Method::GET,
                &format!("{}?limit=2&cursor={cursor}", path(seeded.project.id)),
            )
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await
            .json::<Value>();
        assert_eq!(names(&second), ["c"]);
        assert!(second["next_cursor"].is_null());
    }

    #[tokio::test]
    async fn no_webhook_values_in_logs() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (outcome, logs) = capture_logs(async {
            let created = create(&app, &seeded, &webhook_body("Hooked")).await;
            assert_eq!(created.status, 201, "{}", text(&created));
            let id = id_of(&created.json::<Value>());
            let renamed = patch(&app, &seeded, &id, &json!({"name": "Renamed"})).await;
            assert_eq!(renamed.status, 200);
            renamed.status
        })
        .await;
        assert_eq!(outcome, 200);
        assert!(logs.contains("evaluator created"), "{logs}");
        assert!(logs.contains("evaluator updated"), "{logs}");
        assert!(logs.contains("hooks.example.com"), "{logs}");
        for sealed in [
            WEBHOOK_SECRET,
            "/private/path",
            "query-secret",
            WEBHOOK_HEADER_VALUE,
        ] {
            assert!(!logs.contains(sealed), "logs show {sealed}");
        }
    }

    /// The trace of the test-run tests, and its root, child and evaluation-trace ids.
    const RUN_TRACE: &str = "a1000000000000000000000000000001";
    const EVAL_TRACE: &str = "a1000000000000000000000000000002";
    const ROOT: &str = "b100000000000001";
    const CHILD: &str = "b100000000000002";
    const QUESTION: &str = "What is the refund window?";
    const ANSWER: &str = "30 days.";

    /// A test app that may reach mocks on `127.0.0.1`. The clock is set to the real time before
    /// anything is seeded, so the judge trace's span times and the session cookie agree with it
    /// (`crate::eval::test_run`, § Conventions).
    async fn test_app() -> (TestApp, Seeded) {
        let app = TestApp::with_config(|config| {
            config.security.allow_private_network_egress = true;
        })
        .await;
        let real_now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap();
        app.state.clock.set(real_now);
        let seeded = app.seed().await;
        (app, seeded)
    }

    /// A stored span of `dataset_span(0)`'s shape, with the given ids and payloads.
    fn stored_span(
        trace: &str,
        span: &str,
        parent: Option<&str>,
        input: &str,
        output: &str,
        environment: &str,
    ) -> Span {
        let mut row = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().unwrap();
        row.span_id = span.parse::<SpanId>().unwrap();
        row.parent_span_id = parent.map(|id| id.parse::<SpanId>().unwrap());
        row.input = Some(input.to_owned());
        row.input_format = Some(PayloadFormat::Text);
        row.output = Some(output.to_owned());
        row.output_format = Some(PayloadFormat::Text);
        row.environment = Some(environment.to_owned());
        row
    }

    /// The root span of `RUN_TRACE`: the refund question and its answer.
    fn root_span() -> Span {
        stored_span(RUN_TRACE, ROOT, None, QUESTION, ANSWER, "production")
    }

    /// Creates a connection of `acme` that reaches `mock`, with the default model `gpt-test`.
    async fn mocked_connection(app: &TestApp, cookie: &str, mock: &MockLlm) -> Uuid {
        let response = app
            .request(Method::POST, "/api/v1/orgs/acme/llm-connections")
            .header(header::COOKIE.as_str(), cookie)
            .json(&json!({
                "name": "Mocked",
                "adapter": "openai",
                "api_key": "sk-test-key-0123456789",
                "base_url": mock.base_url(),
                "default_model": "gpt-test"
            }))
            .send()
            .await;
        assert_eq!(response.status, 201, "{}", text(&response));
        Uuid::from_str(&id_of(&response.json::<Value>())).unwrap()
    }

    /// The judge of `fixtures/api/requests/evaluator-create.json`, on `connection` and `gpt-test`.
    fn judge_on(connection: Uuid) -> Value {
        let mut config = judge_body()["config"].clone();
        config["connection_id"] = json!(connection);
        config["model"] = json!("gpt-test");
        json!({
            "kind": "llm_judge",
            "config": config,
            "output_score": {"name": "relevance", "data_type": "numeric"}
        })
    }

    /// An `exact_match` check that writes a boolean named `same`.
    fn same_check() -> Value {
        json!({
            "kind": "check",
            "config": {"check": "exact_match"},
            "output_score": {"name": "same", "data_type": "boolean"}
        })
    }

    /// A trace-mode body with `same_check` over `mapping`.
    fn trace_run(trace: &str, span: Option<&str>, mapping: &Value) -> Value {
        let mut body = json!({
            "evaluator": same_check(),
            "trace_id": trace,
            "variable_mapping": mapping
        });
        if let Some(span) = span {
            body["span_id"] = json!(span);
        }
        body
    }

    /// `POST …/evaluators/test`.
    async fn run_test(app: &TestApp, seeded: &Seeded, cookie: &str, body: &Value) -> TestResponse {
        app.request(Method::POST, &format!("{}/test", path(seeded.project.id)))
            .header(header::COOKIE.as_str(), cookie)
            .json(body)
            .send()
            .await
    }

    /// `run_test` that must answer 200, as JSON.
    async fn run_ok(app: &TestApp, seeded: &Seeded, body: &Value) -> Value {
        let response = run_test(app, seeded, &seeded.cookie, body).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        response.json()
    }

    /// The judge trace's id of an answer.
    fn judge_trace_of(answer: &Value) -> String {
        answer["judge_trace_id"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn judge_test_on_a_trace() {
        let mock = MockLlm::start(vec![completion(
            r#"{"explanation": "Relevant.", "value": 0.9}"#,
        )])
        .await;
        let (app, seeded) = test_app().await;
        let connection = mocked_connection(&app, &seeded.cookie, &mock).await;
        app.commit_spans(&seeded.project, vec![root_span()]).await;

        let answer = run_ok(
            &app,
            &seeded,
            &json!({
                "evaluator": judge_on(connection),
                "trace_id": RUN_TRACE,
                "variable_mapping": {"input": "trace.input", "output": "trace.output"}
            }),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(
            answer["value"],
            json!({"data_type": "numeric", "value": 0.9})
        );
        assert_eq!(answer["reason"], "Relevant.");
        assert_eq!(answer["variables"]["input"], QUESTION);
        assert!(answer["prompt"].as_str().unwrap().contains(QUESTION));
        assert_eq!(answer["usage"]["input_tokens"], 10);
        assert_eq!(answer["trace_truncated"], false);
        let judge = judge_trace_of(&answer);

        let read = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/traces/{judge}", seeded.project.id),
            )
            .header(header::COOKIE.as_str(), &seeded.cookie)
            .send()
            .await;
        assert_eq!(read.status, 200, "{}", text(&read));
        assert!(
            text(&read).contains("llmobs-eval"),
            "the judge trace is in llmobs-eval"
        );

        app.state
            .usage
            .flush(&app.state.meta)
            .await
            .expect("usage flushes");
        let counted: (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(judge_input_tokens), 0), COALESCE(SUM(spans), 0) \
             FROM usage_daily WHERE project_id = ?",
        )
        .bind(seeded.project.id.to_string())
        .fetch_one(app.state.meta.reader())
        .await
        .unwrap();
        assert_eq!(
            counted,
            (10, 1),
            "the target span counts, the judge spans do not"
        );
    }

    #[tokio::test]
    async fn test_with_literal_variables() {
        let mock = MockLlm::start(vec![completion(
            r#"{"explanation": "Relevant.", "value": 0.4}"#,
        )])
        .await;
        let (app, seeded) = test_app().await;
        let connection = mocked_connection(&app, &seeded.cookie, &mock).await;
        let answer = run_ok(
            &app,
            &seeded,
            &json!({
                "evaluator": judge_on(connection),
                "variables": {"input": "a literal question", "output": "a literal answer"}
            }),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(
            answer["value"],
            json!({"data_type": "numeric", "value": 0.4})
        );
        assert_eq!(answer["variables"]["input"], "a literal question");
        assert_eq!(answer["trace_truncated"], false);
        assert!(answer.get("judge_trace_id").is_some());
    }

    #[tokio::test]
    async fn unsaved_evaluator_runs_without_saving() {
        let (app, seeded) = test_app().await;
        let answer = run_ok(
            &app,
            &seeded,
            &json!({"evaluator": same_check(), "variables": {"output": "a", "expected": "a"}}),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        let saved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evaluators")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(saved, 0);
    }

    #[tokio::test]
    async fn check_test_runs_without_a_provider() {
        let (app, seeded) = test_app().await;
        let answer = run_ok(
            &app,
            &seeded,
            &json!({"evaluator": same_check(), "variables": {"output": "a", "expected": "a"}}),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(
            answer["value"],
            json!({"data_type": "boolean", "value": true})
        );
        assert!(answer.get("usage").is_none(), "a check makes no LLM call");
    }

    #[tokio::test]
    async fn mapping_error_has_no_judge_trace() {
        let (app, seeded) = test_app().await;
        app.commit_spans(&seeded.project, vec![root_span()]).await;
        // `span.input` has no target span without `span_id`.
        let answer = run_ok(
            &app,
            &seeded,
            &trace_run(
                RUN_TRACE,
                None,
                &json!({"output": "span.output", "expected": "span.output"}),
            ),
        )
        .await;
        assert_eq!(answer["ok"], false, "{answer}");
        assert_eq!(answer["error"]["class"], "permanent");
        assert!(answer.get("judge_trace_id").is_none());
        assert_eq!(answer["variables"], json!({}));
        assert_eq!(answer["trace_truncated"], false);
    }

    #[tokio::test]
    async fn evaluation_trace_is_refused() {
        let (app, seeded) = test_app().await;
        app.commit_spans(
            &seeded.project,
            vec![stored_span(
                EVAL_TRACE,
                ROOT,
                None,
                "judge input",
                "judge output",
                "llmobs-eval",
            )],
        )
        .await;
        let response = run_test(
            &app,
            &seeded,
            &seeded.cookie,
            &trace_run(
                EVAL_TRACE,
                None,
                &json!({"output": "trace.output", "expected": "trace.output"}),
            ),
        )
        .await;
        assert_eq!(response.status, 400, "{}", text(&response));
        assert!(text(&response).contains("target is an evaluation trace"));
    }

    #[tokio::test]
    async fn unknown_trace_is_404() {
        let (app, seeded) = test_app().await;
        let response = run_test(
            &app,
            &seeded,
            &seeded.cookie,
            &trace_run(
                "a1000000000000000000000000000009",
                None,
                &json!({"output": "trace.output", "expected": "trace.output"}),
            ),
        )
        .await;
        assert_eq!(response.status, 404, "{}", text(&response));
    }

    #[tokio::test]
    async fn provider_failure_is_reported() {
        let mock = MockLlm::start(vec![Reply {
            status: 500,
            body: json!({"error": {"message": "boom"}}),
        }])
        .await;
        let (app, seeded) = test_app().await;
        let connection = mocked_connection(&app, &seeded.cookie, &mock).await;
        let answer = run_ok(
            &app,
            &seeded,
            &json!({
                "evaluator": judge_on(connection),
                "variables": {"input": "q", "output": "a"}
            }),
        )
        .await;
        assert_eq!(answer["ok"], false, "{answer}");
        assert_eq!(answer["error"]["class"], "retryable");
    }

    #[tokio::test]
    async fn long_variables_are_cut() {
        let (app, seeded) = test_app().await;
        let long = "x".repeat(3_000);
        let answer = run_ok(
            &app,
            &seeded,
            &json!({"evaluator": same_check(), "variables": {"output": long, "expected": "x"}}),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        let shown = answer["variables"]["output"].as_str().unwrap();
        assert_eq!(shown.chars().count(), 2_001);
        assert!(shown.ends_with('…'));
        assert_eq!(answer["variables"]["expected"], "x");
    }

    #[tokio::test]
    async fn test_runs_are_rate_limited() {
        let (app, seeded) = test_app().await;
        let body =
            json!({"evaluator": same_check(), "variables": {"output": "a", "expected": "a"}});
        for _ in 0..10 {
            let response = run_test(&app, &seeded, &seeded.cookie, &body).await;
            assert_eq!(response.status, 200, "{}", text(&response));
        }
        let limited = run_test(&app, &seeded, &seeded.cookie, &body).await;
        assert_eq!(limited.status, 429, "{}", text(&limited));
    }

    #[tokio::test]
    async fn viewer_cannot_test() {
        let (app, seeded) = test_app().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let response = run_test(
            &app,
            &seeded,
            &viewer.cookie,
            &json!({"evaluator": same_check(), "variables": {"output": "a", "expected": "a"}}),
        )
        .await;
        assert_eq!(response.status, 403, "{}", text(&response));
    }

    #[tokio::test]
    async fn no_payload_in_logs() {
        let (app, seeded) = test_app().await;
        let (answer, logs) = capture_logs(run_test(
            &app,
            &seeded,
            &seeded.cookie,
            &json!({
                "evaluator": same_check(),
                "variables": {"output": "private-answer-xyz", "expected": "private-answer-xyz"}
            }),
        ))
        .await;
        assert_eq!(answer.status, 200, "{}", text(&answer));
        assert!(logs.contains("evaluator tested"), "{logs}");
        assert!(
            !logs.contains("private-answer-xyz"),
            "logs show the payload: {logs}"
        );
    }

    #[tokio::test]
    async fn webhook_test_signs_with_the_saved_secret() {
        let hook = MockWebhook::start(200, json!({"value": 0.5})).await;
        let (app, seeded) = test_app().await;
        let created = create_ok(
            &app,
            &seeded,
            &json!({
                "name": "Hooked",
                "kind": "webhook",
                "config": {"url": hook.url()},
                "output_score": {"name": "hook", "data_type": "numeric"},
                "webhook_secret": "0123456789abcdef"
            }),
        )
        .await;
        let answer = run_ok(
            &app,
            &seeded,
            &json!({"evaluator_id": id_of(&created), "variables": {"output": "x"}}),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(
            answer["value"],
            json!({"data_type": "numeric", "value": 0.5})
        );
        let requests = hook.requests();
        assert_eq!(requests.len(), 1);
        let signature = requests[0]
            .0
            .get("x-llmobs-signature")
            .expect("a signature header")
            .to_str()
            .unwrap();
        assert!(signature.starts_with("t="), "{signature}");
        assert!(signature.contains("v1="), "{signature}");
    }

    #[tokio::test]
    async fn both_given_takes_the_saved_webhook_values() {
        let hook = MockWebhook::start(200, json!({"value": 0.5})).await;
        let (app, seeded) = test_app().await;
        let created = create_ok(
            &app,
            &seeded,
            &json!({
                "name": "Hooked",
                "kind": "webhook",
                "config": {"url": hook.url()},
                "output_score": {"name": "hook", "data_type": "numeric"},
                "webhook_secret": "0123456789abcdef"
            }),
        )
        .await;
        let answer = run_ok(
            &app,
            &seeded,
            &json!({
                "evaluator_id": id_of(&created),
                "evaluator": {
                    "kind": "webhook",
                    "output_score": {"name": "other", "data_type": "numeric"}
                },
                "variables": {"output": "x"}
            }),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        let requests = hook.requests();
        assert_eq!(requests.len(), 1, "the saved URL was called");
        assert!(requests[0].0.get("x-llmobs-signature").is_some());
    }

    #[tokio::test]
    async fn both_given_with_another_kind_is_400() {
        let hook = MockWebhook::start(200, json!({"value": 0.5})).await;
        let (app, seeded) = test_app().await;
        let created = create_ok(
            &app,
            &seeded,
            &json!({
                "name": "Hooked",
                "kind": "webhook",
                "config": {"url": hook.url()},
                "output_score": {"name": "hook", "data_type": "numeric"},
                "webhook_secret": "0123456789abcdef"
            }),
        )
        .await;
        let response = run_test(
            &app,
            &seeded,
            &seeded.cookie,
            &json!({
                "evaluator_id": id_of(&created),
                "evaluator": {
                    "kind": "check",
                    "output_score": {"name": "same", "data_type": "boolean"}
                },
                "variables": {"output": "a", "expected": "a"}
            }),
        )
        .await;
        assert_eq!(
            field_errors(&response),
            [(
                "evaluator.kind".to_owned(),
                "kind must match the saved evaluator".to_owned()
            )]
        );
        assert!(hook.requests().is_empty());
    }

    #[tokio::test]
    async fn span_target_resolves_span_variables() {
        let (app, seeded) = test_app().await;
        app.commit_spans(
            &seeded.project,
            vec![
                root_span(),
                stored_span(
                    RUN_TRACE,
                    CHILD,
                    Some(ROOT),
                    "child question",
                    "child answer",
                    "production",
                ),
            ],
        )
        .await;
        let answer = run_ok(
            &app,
            &seeded,
            &trace_run(
                RUN_TRACE,
                Some(CHILD),
                &json!({"output": "span.output", "expected": "span.output"}),
            ),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["variables"]["output"], "child answer");
    }

    #[tokio::test]
    async fn trace_start_is_accepted() {
        let (app, seeded) = test_app().await;
        app.commit_spans(&seeded.project, vec![root_span()]).await;
        let start_ms = dataset_span(0).start_time / 1_000_000;
        let mut body = trace_run(
            RUN_TRACE,
            None,
            &json!({"output": "trace.output", "expected": "trace.output"}),
        );
        body["trace_start"] = json!(start_ms.to_string());
        let answer = run_ok(&app, &seeded, &body).await;
        assert_eq!(answer["ok"], true, "{answer}");
    }

    #[tokio::test]
    async fn body_rules() {
        let (app, seeded) = test_app().await;
        let check = same_check();

        let many: serde_json::Map<String, Value> = (0..33)
            .map(|index| (format!("v{index}"), json!("a")))
            .collect();
        let errors = field_errors(
            &run_test(
                &app,
                &seeded,
                &seeded.cookie,
                &json!({"evaluator": check, "variables": many}),
            )
            .await,
        );
        assert!(
            errors.contains(&("variables".to_owned(), "at most 32 variables".to_owned())),
            "{errors:?}"
        );

        let short_trace = trace_run(
            "a100000000000000000000000000001",
            None,
            &json!({"output": "trace.output", "expected": "trace.output"}),
        );
        assert_eq!(
            field_errors(&run_test(&app, &seeded, &seeded.cookie, &short_trace).await),
            [(
                "trace_id".to_owned(),
                "trace_id must be 32 hex digits".to_owned()
            )]
        );

        let span_only = json!({
            "evaluator": same_check(),
            "span_id": ROOT,
            "variables": {"output": "a", "expected": "a"}
        });
        assert_eq!(
            field_errors(&run_test(&app, &seeded, &seeded.cookie, &span_only).await),
            [(
                "trace_id".to_owned(),
                "trace_id is required with span_id and trace_start".to_owned()
            )]
        );

        let neither = json!({"variables": {"output": "a", "expected": "a"}});
        assert_eq!(
            field_errors(&run_test(&app, &seeded, &seeded.cookie, &neither).await),
            [(
                "evaluator".to_owned(),
                "give evaluator_id, evaluator, or both".to_owned()
            )]
        );
    }
}
