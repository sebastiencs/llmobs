//! LLM connection routes (spec 08, spec 09, D-I-4, D-I-24).
//!
//! Every org member reads the org's connections; admins and owners also
//! write, edit and delete them. The API key and the header values are
//! write-only: they are sealed under the row's id and never leave the
//! server again (D-I-2).
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Extension, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::routing::{delete, get, patch, post};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::common::{FieldError, Page, PageParams};
use crate::api::connections::{
    ConnectionStatus, CreateLlmConnectionRequest, LlmAdapter, LlmConnection,
    LlmConnectionTestError, LlmConnectionTestResult, StructuredOutputMode, TestErrorClass,
    TestLlmConnectionRequest, UpdateLlmConnectionRequest,
};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::OrgPerm;
use crate::crypto::{Aad, SecretBox};
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::MetaError;
use crate::meta::connections::{self, ConnectionFields, ConnectionRow};

use super::orgs::page_named;

/// Why a `base_url` was refused, in one sentence (D-I-4).
const BASE_URL_MESSAGE: &str = "base_url must be an http or https URL without credentials, fragment or query other than api-version";
/// At most this many extra headers (D-I-4).
const MAX_EXTRA_HEADERS: usize = 20;
/// The prompt of a connection test's one boolean judge call (D-I-5).
const TEST_PROMPT: &str = "Reply true.";

/// LLM connection routes. Reads need [`OrgPerm::View`]; writes and tests need
/// [`OrgPerm::ManageConnections`] (D-I-24). Not session-only: a PAT works.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs/{org}/llm-connections",
            access: Access::Org {
                perm: OrgPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_llm_connections),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/orgs/{org}/llm-connections",
            access: Access::Org {
                perm: OrgPerm::ManageConnections,
                session_only: false,
            },
            documented: true,
            handler: post(create_llm_connection),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs/{org}/llm-connections/{id}",
            access: Access::Org {
                perm: OrgPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_llm_connection),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/orgs/{org}/llm-connections/{id}",
            access: Access::Org {
                perm: OrgPerm::ManageConnections,
                session_only: false,
            },
            documented: true,
            handler: patch(update_llm_connection),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/orgs/{org}/llm-connections/{id}",
            access: Access::Org {
                perm: OrgPerm::ManageConnections,
                session_only: false,
            },
            documented: true,
            handler: delete(delete_llm_connection),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/orgs/{org}/llm-connections/{id}/test",
            access: Access::Org {
                perm: OrgPerm::ManageConnections,
                session_only: false,
            },
            documented: true,
            handler: post(test_connection),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct IdPath {
    id: Uuid,
}

async fn list_llm_connections(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<LlmConnection>>, ApiError> {
    let rows = connections::list_connections(state.meta.reader(), access.org.id).await?;
    let items = rows
        .iter()
        .map(|row| Ok((row.name.clone(), row.id.to_string(), connection_json(row)?)))
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(error::Json(page_named(
        items,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn get_llm_connection(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
) -> Result<error::Json<LlmConnection>, ApiError> {
    let row = connections::get_connection(state.meta.reader(), access.org.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(error::Json(connection_json(&row)?))
}

async fn create_llm_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Json(body): error::Json<CreateLlmConnectionRequest>,
) -> Result<(StatusCode, error::Json<LlmConnection>), ApiError> {
    let user = principal::session_user(&principal)?;
    let mut errors = Vec::new();
    check_name(&body.name, &mut errors);
    check_api_key(&body.api_key, &mut errors);
    check_base_url(body.base_url.as_deref(), &mut errors);
    check_default_model(body.default_model.as_deref(), &mut errors);
    if let Some(value) = body.max_concurrency {
        check_max_concurrency(value, &mut errors);
    }
    if let Some(value) = body.requests_per_minute {
        check_requests_per_minute(value, &mut errors);
    }
    check_extra_headers(&body.extra_headers, &mut errors);
    if !errors.is_empty() {
        return Err(ApiError::BadRequest {
            detail: None,
            errors,
        });
    }
    let id = Uuid::now_v7();
    let (secret_enc, api_key_hint) = sealed_key(&state.secrets, &id, &body.api_key)?;
    let (extra_headers_enc, extra_header_names) =
        sealed_headers(&state.secrets, &id, &body.extra_headers)?;
    let fields = ConnectionFields {
        name: body.name.trim().to_owned(),
        adapter: body.adapter.as_str().to_owned(),
        base_url: body
            .base_url
            .as_deref()
            .map(str::trim)
            .and_then(empty_to_none),
        secret_enc,
        api_key_hint,
        extra_headers_enc,
        extra_header_names,
        default_model: body
            .default_model
            .as_deref()
            .map(str::trim)
            .and_then(empty_to_none),
        structured_output: body
            .structured_output
            .unwrap_or(StructuredOutputMode::Native)
            .as_str()
            .to_owned(),
        max_concurrency: body.max_concurrency.unwrap_or(4),
        requests_per_minute: body.requests_per_minute.unwrap_or(300),
    };
    let org_id = access.org.id;
    let user_id = user.id;
    let now = state.clock.now_ns();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                connections::insert_connection(conn, id, org_id, fields, Some(user_id), now).await
            })
        })
        .await?;
    tracing::info!(
        target: "audit",
        connection_id = %id,
        org_id = %org_id,
        user_id = %user_id,
        "llm connection created"
    );
    Ok((StatusCode::CREATED, error::Json(connection_json(&row)?)))
}

async fn update_llm_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
    error::Json(body): error::Json<UpdateLlmConnectionRequest>,
) -> Result<error::Json<LlmConnection>, ApiError> {
    let user = principal::session_user(&principal)?;
    let mut errors = Vec::new();
    if let Some(name) = body.name.as_deref() {
        check_name(name, &mut errors);
    }
    if let Some(key) = body.api_key.as_deref() {
        check_api_key(key, &mut errors);
    }
    check_base_url(body.base_url.as_deref(), &mut errors);
    check_default_model(body.default_model.as_deref(), &mut errors);
    if let Some(value) = body.max_concurrency {
        check_max_concurrency(value, &mut errors);
    }
    if let Some(value) = body.requests_per_minute {
        check_requests_per_minute(value, &mut errors);
    }
    if let Some(headers) = body.extra_headers.as_ref() {
        check_extra_headers(headers, &mut errors);
    }
    if !errors.is_empty() {
        return Err(ApiError::BadRequest {
            detail: None,
            errors,
        });
    }
    let UpdateLlmConnectionRequest {
        name,
        adapter,
        api_key,
        base_url,
        default_model,
        structured_output,
        max_concurrency,
        requests_per_minute,
        extra_headers,
    } = body;
    // Sealed before the transaction: the write closure must be `Send` and
    // cannot borrow the SecretBox.
    let key = match api_key.as_deref() {
        Some(key) => Some(sealed_key(&state.secrets, &id, key)?),
        None => None,
    };
    let headers = match extra_headers.as_ref() {
        Some(headers) => Some(sealed_headers(&state.secrets, &id, headers)?),
        None => None,
    };
    let org_id = access.org.id;
    let user_id = user.id;
    let now = state.clock.now_ns();
    let updated = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                // Read, merge and write in one transaction, so two PATCHes
                // cannot undo each other.
                let Some(row) = connections::get_connection_tx(conn, org_id, id).await? else {
                    return Ok(None);
                };
                let fields = ConnectionFields {
                    name: name
                        .map(|value| value.trim().to_owned())
                        .unwrap_or_else(|| row.name.clone()),
                    adapter: adapter
                        .map(|value| value.as_str().to_owned())
                        .unwrap_or_else(|| row.adapter.clone()),
                    base_url: base_url
                        .map(|value| empty_to_none(value.trim()))
                        .unwrap_or_else(|| row.base_url.clone()),
                    secret_enc: key
                        .as_ref()
                        .map(|(sealed, _)| sealed.clone())
                        .unwrap_or_else(|| row.secret_enc.clone()),
                    api_key_hint: key
                        .as_ref()
                        .map(|(_, hint)| hint.clone())
                        .unwrap_or_else(|| row.api_key_hint.clone()),
                    extra_headers_enc: headers
                        .as_ref()
                        .map(|(sealed, _)| sealed.clone())
                        .unwrap_or_else(|| row.extra_headers_enc.clone()),
                    extra_header_names: headers
                        .as_ref()
                        .map(|(_, names)| names.clone())
                        .unwrap_or_else(|| row.extra_header_names.clone()),
                    default_model: default_model
                        .map(|value| empty_to_none(value.trim()))
                        .unwrap_or_else(|| row.default_model.clone()),
                    structured_output: structured_output
                        .map(|value| value.as_str().to_owned())
                        .unwrap_or_else(|| row.structured_output.clone()),
                    max_concurrency: max_concurrency.unwrap_or(row.max_concurrency),
                    requests_per_minute: requests_per_minute.unwrap_or(row.requests_per_minute),
                };
                connections::update_connection(conn, org_id, id, fields, now).await
            })
        })
        .await?;
    let Some(row) = updated else {
        return Err(ApiError::NotFound);
    };
    state.connections.forget(id);
    tracing::info!(
        target: "audit",
        connection_id = %id,
        org_id = %org_id,
        user_id = %user_id,
        "llm connection updated"
    );
    Ok(error::Json(connection_json(&row)?))
}

async fn delete_llm_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
) -> Result<StatusCode, ApiError> {
    let user = principal::session_user(&principal)?;
    let org_id = access.org.id;
    let user_id = user.id;
    let went = state
        .meta
        .write(move |conn| {
            Box::pin(async move { connections::delete_connection(conn, org_id, id).await })
        })
        .await?;
    if !went {
        return Err(ApiError::NotFound);
    }
    state.connections.forget(id);
    tracing::info!(
        target: "audit",
        connection_id = %id,
        org_id = %org_id,
        user_id = %user_id,
        "llm connection deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Runs one boolean judge call through the connection's decrypted key and
/// answers whether it reached the provider (D-I-5). The judge trace is
/// discarded: a connection belongs to no project.
async fn test_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
    error::Json(body): error::Json<TestLlmConnectionRequest>,
) -> Result<error::Json<LlmConnectionTestResult>, ApiError> {
    let user = principal::session_user(&principal)?;
    state
        .limits
        .connection_tests
        .check(&user.id, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    let row = connections::get_connection(state.meta.reader(), access.org.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let explicit = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned);
    let model = explicit
        .clone()
        .or_else(|| row.default_model.clone())
        .ok_or_else(|| ApiError::BadRequest {
            detail: None,
            errors: vec![field_error(
                "model",
                "model is required when the connection has no default model",
            )],
        })?;
    let connection = state
        .connections
        .get(&state.secrets, &row)
        .map_err(internal)?;
    let evaluator = llmobs_eval::Evaluator {
        id: row.id,
        name: "connection test".to_owned(),
        config: llmobs_eval::EvaluatorConfig::LlmJudge(llmobs_eval::JudgeConfig {
            connection_id: row.id,
            model: model.clone(),
            prompt: TEST_PROMPT.to_owned(),
            output: llmobs_eval::JudgeOutput::Boolean,
            include_reason: false,
            temperature: None,
            // Reasoning models spend tokens before answering; 1024 can truncate a working call.
            max_output_tokens: 4096,
            timeout_ms: 30_000,
        }),
        output: llmobs_eval::OutputScore {
            name: "connection_test".to_owned(),
            data_type: llmobs_core::enums::ScoreDataType::Boolean,
            categories: None,
        },
    };
    let ingest = llmobs_otel::IngestContext {
        // A connection belongs to no project, and the judge trace is discarded.
        project_id: llmobs_core::ids::ProjectId::from_uuid(Uuid::nil()),
        ingest_time: state.clock.now_ns(),
        retention_days: None,
        limits: state.config.ingest.limits(&state.config.media),
    };
    let prices = state.ingest.prices();
    let bytes = crate::auth::secret::random_bytes(16).map_err(internal)?;
    let bytes = <[u8; 16]>::try_from(bytes.as_slice()).map_err(internal)?;
    let judge_trace_id = llmobs_core::ids::TraceId::from_bytes(bytes).map_err(internal)?; // refuses all-zero bytes only
    let started = std::time::Instant::now();
    let run = state
        .eval_runner
        .run(llmobs_eval::RunInput {
            evaluator: &evaluator,
            variables: &llmobs_eval::Variables::from_map(BTreeMap::new()),
            connection: Some(connection.as_ref()),
            webhook_secret: None,
            target: llmobs_eval::EvalTarget {
                trace_id: None,
                span_id: None,
            },
            origin: llmobs_eval::EvalOrigin::Test,
            judge_trace_id,
            ingest: &ingest,
            prices: &prices,
        })
        .await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut result = LlmConnectionTestResult {
        ok: true,
        model,
        latency_ms,
        error: None,
    };
    // A pause-class failure says the key, quota or model access was refused.
    let mut pause_class = false;
    if let Err(error) = run.outcome {
        pause_class = error.class() == llmobs_eval::ErrorClass::Pause;
        result.ok = false;
        result.error = Some(LlmConnectionTestError {
            class: match error.class() {
                llmobs_eval::ErrorClass::Retryable => TestErrorClass::Retryable,
                llmobs_eval::ErrorClass::Pause => TestErrorClass::Pause,
                llmobs_eval::ErrorClass::Permanent => TestErrorClass::Permanent,
            },
            // The display quotes no payload (D160).
            message: error.to_string(),
        });
    }
    let now = state.clock.now_ns();
    if result.ok {
        if row.status != "ok" {
            set_status(&state, id, "ok", None, now).await?;
        }
    } else if pause_class && explicit.is_none() {
        // Only a pause on the connection's own default model marks it failing:
        // a 404 for a typed model name says nothing about the key.
        let message = result
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default();
        set_status(&state, id, "failing", Some(message), now).await?;
    }
    tracing::info!(
        target: "audit",
        connection_id = %row.id,
        org_id = %access.org.id,
        user_id = %user.id,
        ok = result.ok,
        latency_ms,
        "llm connection tested"
    );
    Ok(error::Json(result))
}

/// Stores a connection's new `status` without touching `updated_at` (D-I-5).
async fn set_status(
    state: &AppState,
    id: Uuid,
    status: &'static str,
    error: Option<String>,
    now: i64,
) -> Result<(), ApiError> {
    state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                connections::set_connection_status(conn, id, status, error.as_deref(), now).await
            })
        })
        .await?;
    Ok(())
}

/// The stored connection as the API shows it, without the sealed values.
///
/// # Errors
///
/// Returns [`ApiError::Internal`] when a stored name is not one of the
/// connection's own values.
fn connection_json(row: &ConnectionRow) -> Result<LlmConnection, ApiError> {
    let corrupt = |column: &'static str| {
        ApiError::Internal(Box::new(MetaError::Corrupt {
            table: "llm_connections",
            column,
        }))
    };
    Ok(LlmConnection {
        id: row.id,
        name: row.name.clone(),
        adapter: LlmAdapter::from_name(&row.adapter).ok_or_else(|| corrupt("adapter"))?,
        base_url: row.base_url.clone(),
        default_model: row.default_model.clone(),
        structured_output: StructuredOutputMode::from_name(&row.structured_output)
            .ok_or_else(|| corrupt("structured_output"))?,
        max_concurrency: row.max_concurrency,
        requests_per_minute: row.requests_per_minute,
        api_key_hint: row.api_key_hint.clone(),
        extra_header_names: row.extra_header_names.clone(),
        status: ConnectionStatus::from_name(&row.status).ok_or_else(|| corrupt("status"))?,
        last_error: row.last_error.clone(),
        last_error_at: row.last_error_at,
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Seals the API key for the row `id`, with the hint shown in its place.
fn sealed_key(secrets: &SecretBox, id: &Uuid, key: &str) -> Result<(Vec<u8>, String), ApiError> {
    let sealed = secrets
        .seal(
            key.as_bytes(),
            Aad {
                table: "llm_connections",
                column: "secret_enc",
                row_id: id,
            },
        )
        .map_err(internal)?;
    Ok((sealed, key_hint(key)))
}

/// Seals the extra headers for the row `id`, keyed by their lowercased names,
/// and lists those names in the map's order.
fn sealed_headers(
    secrets: &SecretBox,
    id: &Uuid,
    headers: &BTreeMap<String, String>,
) -> Result<(Option<Vec<u8>>, Vec<String>), ApiError> {
    if headers.is_empty() {
        return Ok((None, Vec::new()));
    }
    let map: BTreeMap<String, String> = headers
        .iter()
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.clone()))
        .collect();
    let names = map.keys().cloned().collect();
    let bytes = serde_json::to_vec(&map).map_err(internal)?;
    let sealed = secrets
        .seal(
            &bytes,
            Aad {
                table: "llm_connections",
                column: "extra_headers_enc",
                row_id: id,
            },
        )
        .map_err(internal)?;
    Ok((Some(sealed), names))
}

/// The key's last 4 characters when it has at least 16, else an empty
/// string (D-I-4).
fn key_hint(key: &str) -> String {
    let count = key.chars().count();
    if count >= 16 {
        key.chars().skip(count - 4).collect()
    } else {
        String::new()
    }
}

/// A trimmed text; an empty result stays unset.
fn empty_to_none(trimmed: &str) -> Option<String> {
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn internal(error: impl std::error::Error + Send + Sync + 'static) -> ApiError {
    ApiError::Internal(Box::new(error))
}

/// `name`: 1 to 100 characters after trimming.
fn check_name(name: &str, errors: &mut Vec<FieldError>) {
    let length = name.trim().chars().count();
    if !(1..=100).contains(&length) {
        errors.push(field_error("name", "name must be 1 to 100 characters"));
    }
}

/// `api_key`: 1 to 4096 bytes, no whitespace at either end, no control
/// character.
fn check_api_key(key: &str, errors: &mut Vec<FieldError>) {
    if key.is_empty()
        || key.len() > 4096
        || key.starts_with(char::is_whitespace)
        || key.ends_with(char::is_whitespace)
        || key.chars().any(char::is_control)
    {
        errors.push(field_error(
            "api_key",
            "api_key must be 1 to 4096 bytes, without spaces at either end",
        ));
    }
}

/// `base_url` when it carries a value: an http or https URL without
/// credentials, fragment, or a query other than one `api-version` pair.
/// An empty value clears the field and is not checked.
fn check_base_url(value: Option<&str>, errors: &mut Vec<FieldError>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if base_url_is_valid(value) {
        return;
    }
    errors.push(field_error("base_url", BASE_URL_MESSAGE));
}

fn base_url_is_valid(value: &str) -> bool {
    if value.len() > 2048 {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    // Other query keys are refused because providers put keys there (?key=,
    // ?code=), and `base_url` is shown to every org member.
    let query_ok = match url.query() {
        None => true,
        Some(_) => {
            let pairs: Vec<_> = url.query_pairs().collect();
            pairs.len() == 1 && pairs[0].0 == "api-version"
        }
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && authority_is_written(value, &url)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && query_ok
}

/// `url` forgives an empty authority (`http:///v1` parses with host `v1`),
/// so the host must also be written in the input.
fn authority_is_written(value: &str, url: &url::Url) -> bool {
    let rest = value
        .get(url.scheme().len()..)
        .and_then(|rest| rest.strip_prefix("://"))
        .unwrap_or("");
    rest.chars()
        .next()
        .is_some_and(|ch| ch != '/' && ch != '\\')
}

/// `default_model` when it carries a value: the eval crate's own model rule,
/// 1 to 200 bytes. An empty value clears the field and is not checked.
fn check_default_model(model: Option<&str>, errors: &mut Vec<FieldError>) {
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return;
    };
    let trimmed = model.trim();
    let forbidden = trimmed.chars().any(|ch| {
        ch.is_whitespace() || ch.is_control() || matches!(ch, '/' | '\\' | '%' | '?' | '#')
    });
    if !(1..=200).contains(&trimmed.len()) || forbidden {
        errors.push(field_error(
            "default_model",
            "default_model must be 1 to 200 characters without spaces or / \\ % ? #",
        ));
    }
}

fn check_max_concurrency(value: u32, errors: &mut Vec<FieldError>) {
    if !(1..=64).contains(&value) {
        errors.push(field_error(
            "max_concurrency",
            "max_concurrency must be between 1 and 64",
        ));
    }
}

fn check_requests_per_minute(value: u32, errors: &mut Vec<FieldError>) {
    if !(1..=10_000).contains(&value) {
        errors.push(field_error(
            "requests_per_minute",
            "requests_per_minute must be between 1 and 10000",
        ));
    }
}

/// `extra_headers`: at most 20 entries, names HTTP accepts that are not
/// lowercased duplicates and not headers the provider connection owns, and
/// header-safe values of at most 4096 bytes.
fn check_extra_headers(headers: &BTreeMap<String, String>, errors: &mut Vec<FieldError>) {
    if headers.len() > MAX_EXTRA_HEADERS {
        errors.push(field_error("extra_headers", "at most 20 extra headers"));
        return;
    }
    let mut seen = BTreeSet::new();
    for (name, value) in headers {
        let trimmed = name.trim();
        if HeaderName::from_bytes(trimmed.as_bytes()).is_err() {
            errors.push(field_error("extra_headers", "invalid header name"));
            continue;
        }
        let lowercase = trimmed.to_ascii_lowercase();
        let owned = matches!(
            lowercase.as_str(),
            "content-type" | "content-length" | "host" | "transfer-encoding"
        );
        let duplicate = !seen.insert(lowercase.clone());
        let value_ok = value.len() <= 4096 && HeaderValue::from_str(value).is_ok();
        if owned || duplicate || !value_ok {
            errors.push(field_error(
                &format!("extra_headers.{lowercase}"),
                "invalid header",
            ));
        }
    }
}

fn field_error(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.to_owned(),
        position: None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::{HeaderMap, Method, header};
    use llmobs_core::ids::OrgId;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::*;
    use crate::api::account::OrgRole;
    use crate::auth::session::start_session;
    use crate::meta::members::add_org_member;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::mock_llm::{MockLlm, Reply, completion};
    use crate::test_support::{TestApp, TestResponse, capture_logs};

    fn cookie_pair(set_cookie: &HeaderValue) -> String {
        set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn signed_in(app: &TestApp, email: &str) -> (Uuid, String) {
        let email = email.to_owned();
        let now = app.state.clock.now_ns();
        let user = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email,
                            name: None,
                            password_hash: None,
                            email_verified_at: Some(now),
                            is_instance_admin: false,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let cookie = start_session(&app.state, user.id, None, None)
            .await
            .unwrap();
        (user.id, cookie_pair(&cookie))
    }

    async fn add_member(app: &TestApp, org: OrgId, user: Uuid, role: OrgRole) {
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { add_org_member(conn, org, user, role, now).await })
            })
            .await
            .unwrap();
    }

    /// A fresh app with org `acme` and its owner's cookie.
    async fn app() -> (TestApp, String) {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        (app, seeded.cookie)
    }

    /// [`app`] with private-network egress allowed, so judges can reach the
    /// mock provider on `127.0.0.1`.
    async fn egress_app() -> (TestApp, String) {
        let app = TestApp::with_config(|config| {
            config.security.allow_private_network_egress = true;
        })
        .await;
        let seeded = app.seed().await;
        (app, seeded.cookie)
    }

    async fn create(app: &TestApp, cookie: &str, body: &Value) -> TestResponse {
        app.request(Method::POST, "/api/v1/orgs/acme/llm-connections")
            .header(header::COOKIE.as_str(), cookie)
            .json(body)
            .send()
            .await
    }

    async fn patch(app: &TestApp, cookie: &str, id: Uuid, body: &Value) -> TestResponse {
        app.request(
            Method::PATCH,
            &format!("/api/v1/orgs/acme/llm-connections/{id}"),
        )
        .header(header::COOKIE.as_str(), cookie)
        .json(body)
        .send()
        .await
    }

    async fn get(app: &TestApp, cookie: &str, id: Uuid) -> TestResponse {
        app.request(
            Method::GET,
            &format!("/api/v1/orgs/acme/llm-connections/{id}"),
        )
        .header(header::COOKIE.as_str(), cookie)
        .send()
        .await
    }

    async fn delete(app: &TestApp, cookie: &str, id: Uuid) -> TestResponse {
        app.request(
            Method::DELETE,
            &format!("/api/v1/orgs/acme/llm-connections/{id}"),
        )
        .header(header::COOKIE.as_str(), cookie)
        .send()
        .await
    }

    async fn test(app: &TestApp, cookie: &str, id: Uuid, body: &Value) -> TestResponse {
        app.request(
            Method::POST,
            &format!("/api/v1/orgs/acme/llm-connections/{id}/test"),
        )
        .header(header::COOKIE.as_str(), cookie)
        .json(body)
        .send()
        .await
    }

    /// A connection pointing at `base_url` with the default model `gpt-test`.
    async fn mocked(app: &TestApp, cookie: &str, mock: &MockLlm) -> Uuid {
        let created = create(
            app,
            cookie,
            &json!({
                "name": "Mocked",
                "adapter": "openai",
                "api_key": "sk-test-key-0123456789",
                "base_url": mock.base_url(),
                "default_model": "gpt-test"
            }),
        )
        .await;
        assert_eq!(created.status, 201, "{}", text(&created));
        created_id(&created)
    }

    /// The header value, when it is one valid string.
    fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap()
    }

    fn body(name: &str) -> Value {
        json!({ "name": name, "adapter": "openai", "api_key": "sk-test-0123456789abcdef" })
    }

    fn created_id(response: &TestResponse) -> Uuid {
        response.json::<LlmConnection>().id
    }

    /// The stored `secret_enc` of `id`.
    async fn stored_secret(app: &TestApp, id: Uuid) -> Vec<u8> {
        sqlx::query_scalar::<_, Option<Vec<u8>>>(
            "SELECT secret_enc FROM llm_connections WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_one(app.state.meta.reader())
        .await
        .unwrap()
        .expect("a sealed key")
    }

    /// The stored `extra_headers_enc` of `id`; `None` when the column is NULL.
    async fn stored_headers(app: &TestApp, id: Uuid) -> Option<Vec<u8>> {
        sqlx::query_scalar::<_, Option<Vec<u8>>>(
            "SELECT extra_headers_enc FROM llm_connections WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_one(app.state.meta.reader())
        .await
        .unwrap()
    }

    fn text(response: &TestResponse) -> String {
        String::from_utf8_lossy(&response.body).into_owned()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn api_key_rules() {
        let (app, cookie) = app().await;
        let too_long = format!("sk-{}", "x".repeat(4097 - 3));
        for bad in [too_long.as_str(), " sk-x", "sk-x ", "sk\u{7}x"] {
            let response = create(
                &app,
                &cookie,
                &json!({ "name": "A", "adapter": "openai", "api_key": bad }),
            )
            .await;
            assert_eq!(response.status, 400, "{bad}");
            let problem = response.problem();
            assert_eq!(problem.errors.len(), 1);
            assert_eq!(problem.errors[0].field, "api_key");
        }
        let ok = create(
            &app,
            &cookie,
            &json!({ "name": "A", "adapter": "openai", "api_key": "x".repeat(4096) }),
        )
        .await;
        assert_eq!(ok.status, 201, "{}", text(&ok));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn base_url_rules() {
        let (app, cookie) = app().await;
        let too_long = format!(
            "https://x.com/{}",
            "a".repeat(2049 - "https://x.com/".len())
        );
        for bad in [
            "https://x.com/v1#a",
            "http:///v1",
            too_long.as_str(),
            "https://x.com/v1?key=abc",
        ] {
            let response = create(
                &app,
                &cookie,
                &json!({ "name": "A", "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "base_url": bad }),
            )
            .await;
            assert_eq!(response.status, 400, "{bad}");
            let problem = response.problem();
            assert_eq!(problem.errors.len(), 1);
            assert_eq!(problem.errors[0].field, "base_url");
        }
        let ok = create(
            &app,
            &cookie,
            &json!({
                "name": "A",
                "adapter": "openai",
                "api_key": "sk-test-0123456789abcdef",
                "base_url": "https://x.com/v1?api-version=2024-10-21"
            }),
        )
        .await;
        assert_eq!(ok.status, 201, "{}", text(&ok));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrency_bounds() {
        let (app, cookie) = app().await;
        for good in [1, 64] {
            let response = create(
                &app,
                &cookie,
                &json!({ "name": format!("Bounds-{good}"), "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "max_concurrency": good }),
            )
            .await;
            assert_eq!(response.status, 201, "{good}");
        }
        let bad = create(
            &app,
            &cookie,
            &json!({ "name": "B", "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "max_concurrency": 65 }),
        )
        .await;
        assert_eq!(bad.status, 400);
        assert_eq!(bad.problem().errors[0].field, "max_concurrency");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn patch_name_conflict_is_409() {
        let (app, cookie) = app().await;
        let first = create(&app, &cookie, &body("First")).await;
        assert_eq!(first.status, 201);
        let second = create(&app, &cookie, &body("Second")).await;
        assert_eq!(second.status, 201);
        let id = created_id(&second);
        let response = patch(&app, &cookie, id, &json!({ "name": "First" })).await;
        assert_eq!(response.status, 409);
        assert_eq!(
            response.problem().detail.as_deref(),
            Some("a connection with this name already exists")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn other_org_patch_and_delete_are_404() {
        let (app, cookie) = app().await;
        let other = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&json!({ "name": "Other" }))
            .send()
            .await;
        assert_eq!(other.status, 201, "{}", text(&other));
        let slug = other.json::<Value>()["slug"].as_str().unwrap().to_owned();
        let created = app
            .request(
                Method::POST,
                &format!("/api/v1/orgs/{slug}/llm-connections"),
            )
            .header(header::COOKIE.as_str(), &cookie)
            .json(&body("Elsewhere"))
            .send()
            .await;
        assert_eq!(created.status, 201, "{}", text(&created));
        let id = created_id(&created);

        let patched = patch(&app, &cookie, id, &json!({ "name": "Renamed" })).await;
        assert_eq!(patched.status, 404);
        let deleted = delete(&app, &cookie, id).await;
        assert_eq!(deleted.status, 404);

        let still = app
            .request(
                Method::GET,
                &format!("/api/v1/orgs/{slug}/llm-connections/{id}"),
            )
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        assert_eq!(still.status, 200);
        assert_eq!(still.json::<LlmConnection>().name, "Elsewhere");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_pages_by_name() {
        let (app, cookie) = app().await;
        for name in ["c", "a", "b"] {
            let response = create(&app, &cookie, &body(name)).await;
            assert_eq!(response.status, 201);
        }
        let first = app
            .request(Method::GET, "/api/v1/orgs/acme/llm-connections?limit=1")
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        assert_eq!(first.status, 200);
        let first: Page<LlmConnection> = first.json();
        assert_eq!(first.data[0].name, "a");
        let cursor = first.next_cursor.expect("another page");

        let second = app
            .request(
                Method::GET,
                &format!("/api/v1/orgs/acme/llm-connections?limit=1&cursor={cursor}"),
            )
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        let second: Page<LlmConnection> = second.json();
        assert_eq!(second.data[0].name, "b");
        let cursor = second.next_cursor.expect("a last page");

        let third = app
            .request(
                Method::GET,
                &format!("/api/v1/orgs/acme/llm-connections?limit=1&cursor={cursor}"),
            )
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        let third: Page<LlmConnection> = third.json();
        assert_eq!(third.data[0].name, "c");
        assert!(third.next_cursor.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn owner_creates_and_reads_back_without_the_key() {
        let (app, cookie) = app().await;
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/api/requests/llm-connection-create.json"
        );
        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
        let created = create(&app, &cookie, &request).await;
        assert_eq!(created.status, 201, "{}", text(&created));
        let response_text = text(&created);
        assert!(!response_text.contains("sk-EXAMPLE"), "{response_text}");
        assert!(!response_text.contains("\"api_key\""), "{response_text}");
        let connection: LlmConnection = created.json();
        assert_eq!(connection.api_key_hint, "WXYZ");
        assert_eq!(connection.name, "OpenAI");

        let got = get(&app, &cookie, connection.id).await;
        assert_eq!(got.status, 200);
        assert_eq!(got.json::<LlmConnection>(), connection);

        let list = app
            .request(Method::GET, "/api/v1/orgs/acme/llm-connections")
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        assert_eq!(list.status, 200);
        let page: Page<LlmConnection> = list.json();
        assert_eq!(page.data, vec![connection]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_stored_key_is_sealed_with_its_row_id() {
        let (app, cookie) = app().await;
        let key = "sk-sealed-0123456789";
        let created = create(
            &app,
            &cookie,
            &json!({ "name": "Sealed", "adapter": "openai", "api_key": key }),
        )
        .await;
        assert_eq!(created.status, 201, "{}", text(&created));
        let id = created_id(&created);
        let stored = stored_secret(&app, id).await;
        assert!(
            !stored
                .windows(key.len())
                .any(|window| window == key.as_bytes()),
            "the plaintext key is in the stored bytes"
        );
        let opened = app
            .state
            .secrets
            .open(
                &stored,
                Aad {
                    table: "llm_connections",
                    column: "secret_enc",
                    row_id: &id,
                },
            )
            .unwrap();
        assert_eq!(opened, key.as_bytes());
        let moved = app.state.secrets.open(
            &stored,
            Aad {
                table: "llm_connections",
                column: "secret_enc",
                row_id: &Uuid::now_v7(),
            },
        );
        assert!(moved.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hint_boundaries() {
        let (app, cookie) = app().await;
        let short = create(
            &app,
            &cookie,
            &json!({ "name": "Short", "adapter": "openai", "api_key": "x".repeat(15) }),
        )
        .await;
        assert_eq!(short.json::<LlmConnection>().api_key_hint, "");
        let sixteen = create(
            &app,
            &cookie,
            &json!({ "name": "Sixteen", "adapter": "openai", "api_key": "abcdefghijklmnop" }),
        )
        .await;
        assert_eq!(sixteen.json::<LlmConnection>().api_key_hint, "mnop");
        let accents = create(
            &app,
            &cookie,
            &json!({ "name": "Accents", "adapter": "openai", "api_key": format!("{}éèàü", "k".repeat(12)) }),
        )
        .await;
        assert_eq!(accents.json::<LlmConnection>().api_key_hint, "éèàü");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn validation_reports_every_field() {
        let (app, cookie) = app().await;
        let mut headers = serde_json::Map::new();
        for index in 0..21 {
            headers.insert(format!("x-{index:02}"), Value::String("v".to_owned()));
        }
        let request = json!({
            "name": "  ",
            "adapter": "openai",
            "api_key": "sk-test-0123456789abcdef",
            "base_url": "ftp://x",
            "default_model": "a/b",
            "max_concurrency": 0,
            "requests_per_minute": 20000,
            "extra_headers": Value::Object(headers),
        });
        let response = create(&app, &cookie, &request).await;
        assert_eq!(response.status, 400, "{}", text(&response));
        let problem = response.problem();
        let mut got: Vec<(String, String)> = problem
            .errors
            .iter()
            .map(|error| (error.field.clone(), error.message.clone()))
            .collect();
        got.sort();
        let mut expected: Vec<(String, String)> = [
            ("name", "name must be 1 to 100 characters"),
            ("base_url", "base_url must be an http or https URL without credentials, fragment or query other than api-version"),
            ("default_model", "default_model must be 1 to 200 characters without spaces or / \\ % ? #"),
            ("max_concurrency", "max_concurrency must be between 1 and 64"),
            ("requests_per_minute", "requests_per_minute must be between 1 and 10000"),
            ("extra_headers", "at most 20 extra headers"),
        ]
        .into_iter()
        .map(|(field, message)| (field.to_owned(), message.to_owned()))
        .collect();
        expected.sort();
        assert_eq!(got, expected);

        let list = app
            .request(Method::GET, "/api/v1/orgs/acme/llm-connections")
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        let page: Page<LlmConnection> = list.json();
        assert!(page.data.is_empty(), "nothing was stored");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn base_url_credentials_are_refused() {
        let (app, cookie) = app().await;
        let response = create(
            &app,
            &cookie,
            &json!({
                "name": "A",
                "adapter": "openai",
                "api_key": "sk-test-0123456789abcdef",
                "base_url": "https://u:p@x.com"
            }),
        )
        .await;
        assert_eq!(response.status, 400);
        let problem = response.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "base_url");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn header_rules() {
        let (app, cookie) = app().await;
        let owned = create(
            &app,
            &cookie,
            &json!({ "name": "A", "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "extra_headers": { "Content-Type": "application/json" } }),
        )
        .await;
        assert_eq!(owned.status, 400);
        let problem = owned.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "extra_headers.content-type");
        assert_eq!(problem.errors[0].message, "invalid header");

        let bad_name = create(
            &app,
            &cookie,
            &json!({ "name": "A", "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "extra_headers": { "bad name": "v" } }),
        )
        .await;
        assert_eq!(bad_name.status, 400);
        let problem = bad_name.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "extra_headers");
        assert_eq!(problem.errors[0].message, "invalid header name");

        let duplicate = create(
            &app,
            &cookie,
            &json!({ "name": "A", "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "extra_headers": { "X-A": "1", "x-a": "2" } }),
        )
        .await;
        assert_eq!(duplicate.status, 400);
        let problem = duplicate.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "extra_headers.x-a");

        let ok = create(
            &app,
            &cookie,
            &json!({ "name": "A", "adapter": "openai", "api_key": "sk-test-0123456789abcdef", "extra_headers": { "X-B": "2", "x-a": "1" } }),
        )
        .await;
        assert_eq!(ok.status, 201, "{}", text(&ok));
        let connection: LlmConnection = ok.json();
        assert_eq!(connection.extra_header_names, ["x-a", "x-b"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_name_is_409() {
        let (app, cookie) = app().await;
        let first = create(&app, &cookie, &body("Twin")).await;
        assert_eq!(first.status, 201);
        let second = create(&app, &cookie, &body("Twin")).await;
        assert_eq!(second.status, 409);
        assert_eq!(
            second.problem().detail.as_deref(),
            Some("a connection with this name already exists")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn patch_keeps_absent_fields_and_clears_with_empty_strings() {
        let (app, cookie) = app().await;
        let first_key = "sk-first-0123456789";
        let created = create(
            &app,
            &cookie,
            &json!({
                "name": "Keep",
                "adapter": "openai",
                "api_key": first_key,
                "default_model": "gpt-5-mini"
            }),
        )
        .await;
        assert_eq!(created.status, 201, "{}", text(&created));
        let connection: LlmConnection = created.json();
        let id = connection.id;
        assert_eq!(connection.api_key_hint, "6789");

        let cleared = patch(&app, &cookie, id, &json!({ "default_model": "" })).await;
        assert_eq!(cleared.status, 200, "{}", text(&cleared));
        let cleared: LlmConnection = cleared.json();
        assert_eq!(cleared.default_model, None);
        assert_eq!(cleared.api_key_hint, "6789");
        let stored = stored_secret(&app, id).await;
        let opened = app
            .state
            .secrets
            .open(
                &stored,
                Aad {
                    table: "llm_connections",
                    column: "secret_enc",
                    row_id: &id,
                },
            )
            .unwrap();
        assert_eq!(opened, first_key.as_bytes());

        let rekeyed = patch(
            &app,
            &cookie,
            id,
            &json!({ "api_key": "sk-NEWNEWNEWNEWNEW1234" }),
        )
        .await;
        assert_eq!(rekeyed.status, 200, "{}", text(&rekeyed));
        let rekeyed: LlmConnection = rekeyed.json();
        assert_eq!(rekeyed.api_key_hint, "1234");

        app.state.clock.advance(Duration::from_secs(1));
        let untouched = patch(&app, &cookie, id, &json!({})).await;
        assert_eq!(untouched.status, 200, "{}", text(&untouched));
        let untouched: LlmConnection = untouched.json();
        let mut expected = rekeyed.clone();
        expected.updated_at = untouched.updated_at;
        assert_eq!(untouched, expected);
        assert_eq!(untouched.updated_at, rekeyed.updated_at + 1_000_000_000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn patch_only_checks_present_fields() {
        let (app, cookie) = app().await;
        let created = create(&app, &cookie, &body("Stored")).await;
        assert_eq!(created.status, 201);
        let id = created_id(&created);
        let response = patch(&app, &cookie, id, &json!({ "max_concurrency": 0 })).await;
        assert_eq!(response.status, 400);
        let problem = response.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "max_concurrency");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn patch_unknown_field_is_400() {
        let (app, cookie) = app().await;
        let created = create(&app, &cookie, &body("Stored")).await;
        let id = created_id(&created);
        let response = patch(&app, &cookie, id, &json!({ "api_keyy": "x" })).await;
        assert_eq!(response.status, 400);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn patch_extra_headers_replaces_and_clears() {
        let (app, cookie) = app().await;
        let created = create(
            &app,
            &cookie,
            &json!({
                "name": "Headers",
                "adapter": "openai",
                "api_key": "sk-test-0123456789abcdef",
                "extra_headers": { "x-a": "1", "x-b": "2" }
            }),
        )
        .await;
        assert_eq!(created.status, 201);
        let connection: LlmConnection = created.json();
        let id = connection.id;
        assert_eq!(connection.extra_header_names, ["x-a", "x-b"]);

        let replaced = patch(
            &app,
            &cookie,
            id,
            &json!({ "extra_headers": { "x-c": "3" } }),
        )
        .await;
        assert_eq!(replaced.status, 200, "{}", text(&replaced));
        let replaced: LlmConnection = replaced.json();
        assert_eq!(replaced.extra_header_names, ["x-c"]);
        let stored = stored_headers(&app, id).await.expect("a sealed map");
        let opened = app
            .state
            .secrets
            .open(
                &stored,
                Aad {
                    table: "llm_connections",
                    column: "extra_headers_enc",
                    row_id: &id,
                },
            )
            .unwrap();
        let map: BTreeMap<String, String> = serde_json::from_slice(&opened).unwrap();
        assert_eq!(map, BTreeMap::from([("x-c".to_owned(), "3".to_owned())]));

        let cleared = patch(&app, &cookie, id, &json!({ "extra_headers": {} })).await;
        assert_eq!(cleared.status, 200);
        let cleared: LlmConnection = cleared.json();
        assert!(cleared.extra_header_names.is_empty());
        assert_eq!(stored_headers(&app, id).await, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn member_can_list_but_not_write() {
        let (app, cookie) = app().await;
        let created = create(&app, &cookie, &body("Stored")).await;
        let id = created_id(&created);
        let org = crate::meta::orgs::org_by_slug(app.state.meta.reader(), "acme")
            .await
            .unwrap()
            .expect("the seeded org");
        let (member_id, member) = signed_in(&app, "member@example.com").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;

        let listed = app
            .request(Method::GET, "/api/v1/orgs/acme/llm-connections")
            .header(header::COOKIE.as_str(), &member)
            .send()
            .await;
        assert_eq!(listed.status, 200);
        let page: Page<LlmConnection> = listed.json();
        assert_eq!(page.data.len(), 1);

        let post = create(&app, &member, &body("Nope")).await;
        assert_eq!(post.status, 403);
        let patched = patch(&app, &member, id, &json!({ "name": "Nope" })).await;
        assert_eq!(patched.status, 403);
        let deleted = delete(&app, &member, id).await;
        assert_eq!(deleted.status, 403);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn viewer_can_list() {
        let (app, _cookie) = app().await;
        let org = crate::meta::orgs::org_by_slug(app.state.meta.reader(), "acme")
            .await
            .unwrap()
            .expect("the seeded org");
        let (viewer_id, viewer) = signed_in(&app, "viewer@example.com").await;
        add_member(&app, org.id, viewer_id, OrgRole::Viewer).await;
        let listed = app
            .request(Method::GET, "/api/v1/orgs/acme/llm-connections")
            .header(header::COOKIE.as_str(), &viewer)
            .send()
            .await;
        assert_eq!(listed.status, 200);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn other_org_is_404() {
        let (app, cookie) = app().await;
        let other = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&json!({ "name": "Other" }))
            .send()
            .await;
        assert_eq!(other.status, 201, "{}", text(&other));
        let slug = other.json::<Value>()["slug"].as_str().unwrap().to_owned();
        let created = app
            .request(
                Method::POST,
                &format!("/api/v1/orgs/{slug}/llm-connections"),
            )
            .header(header::COOKIE.as_str(), &cookie)
            .json(&body("Elsewhere"))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let id = created_id(&created);
        let response = get(&app, &cookie, id).await;
        assert_eq!(response.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_then_get_is_404() {
        let (app, cookie) = app().await;
        let created = create(&app, &cookie, &body("Gone")).await;
        let id = created_id(&created);
        let deleted = delete(&app, &cookie, id).await;
        assert_eq!(deleted.status, 204);
        let fetched = get(&app, &cookie, id).await;
        assert_eq!(fetched.status, 404);
        let again = delete(&app, &cookie, id).await;
        assert_eq!(again.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pat_can_manage() {
        let (app, cookie) = app().await;
        let minted = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&json!({ "name": "t", "scopes": ["read", "write"] }))
            .send()
            .await;
        assert_eq!(minted.status, 201, "{}", text(&minted));
        let secret = minted.json::<Value>()["secret"]
            .as_str()
            .unwrap()
            .to_owned();
        let response = app
            .request(Method::POST, "/api/v1/orgs/acme/llm-connections")
            .header(header::AUTHORIZATION.as_str(), &format!("Bearer {secret}"))
            .json(&body("From a PAT"))
            .send()
            .await;
        assert_eq!(response.status, 201, "{}", text(&response));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_key_material_in_logs() {
        let (app, cookie) = app().await;
        let first = "sk-logcheck-one-01234567";
        let second = "sk-logcheck-two-01234567";
        let (_, logs) = capture_logs(async {
            let created = create(
                &app,
                &cookie,
                &json!({ "name": "Logged", "adapter": "openai", "api_key": first }),
            )
            .await;
            assert_eq!(created.status, 201);
            let id = created_id(&created);
            let patched = patch(&app, &cookie, id, &json!({ "api_key": second })).await;
            assert_eq!(patched.status, 200);
        })
        .await;
        assert!(!logs.contains(first), "{logs}");
        assert!(!logs.contains(second), "{logs}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_succeeds_and_uses_the_decrypted_key() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        let (app, cookie) = egress_app().await;
        let created = create(
            &app,
            &cookie,
            &json!({
                "name": "Mocked",
                "adapter": "openai",
                "api_key": "sk-test-key-0123456789",
                "base_url": mock.base_url(),
                "default_model": "gpt-test",
                "extra_headers": { "x-extra": "one" }
            }),
        )
        .await;
        assert_eq!(created.status, 201, "{}", text(&created));
        let id = created_id(&created);

        let response = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        let result: LlmConnectionTestResult = response.json();
        assert!(result.ok);
        assert_eq!(result.model, "gpt-test");
        assert!(result.error.is_none());

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let (headers, body) = &requests[0];
        assert_eq!(
            header_value(headers, "authorization"),
            "Bearer sk-test-key-0123456789"
        );
        assert_eq!(header_value(headers, "x-extra"), "one");
        assert_eq!(body["model"], "gpt-test");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_uses_the_body_model() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;

        let response = test(&app, &cookie, id, &json!({ "model": "other-model" })).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        let result: LlmConnectionTestResult = response.json();
        assert!(result.ok);
        assert_eq!(result.model, "other-model");
        assert_eq!(mock.requests()[0].1["model"], "other-model");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_without_any_model_is_400() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        let (app, cookie) = egress_app().await;
        let created = create(
            &app,
            &cookie,
            &json!({
                "name": "No default",
                "adapter": "openai",
                "api_key": "sk-test-key-0123456789",
                "base_url": mock.base_url()
            }),
        )
        .await;
        assert_eq!(created.status, 201, "{}", text(&created));
        let id = created_id(&created);

        let response = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(response.status, 400);
        let problem = response.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "model");
        assert_eq!(
            problem.errors[0].message,
            "model is required when the connection has no default model"
        );
        assert!(mock.requests().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_401_marks_the_connection_failing() {
        let mock = MockLlm::start(vec![
            Reply {
                status: 401,
                body: json!({
                    "error": {"message": "bad key SECRETPAYLOAD", "type": "invalid_request_error"}
                }),
            },
            completion(r#"{"value": true}"#),
        ])
        .await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;

        let failed = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(failed.status, 200, "{}", text(&failed));
        let result: LlmConnectionTestResult = failed.json();
        assert!(!result.ok);
        let error = result.error.expect("the failure");
        assert_eq!(error.class, TestErrorClass::Pause);
        assert!(
            !error.message.contains("SECRETPAYLOAD"),
            "{}",
            error.message
        );

        let connection: LlmConnection = get(&app, &cookie, id).await.json();
        assert_eq!(connection.status, ConnectionStatus::Failing);
        assert!(connection.last_error.is_some());

        let passed = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(passed.status, 200, "{}", text(&passed));
        let result: LlmConnectionTestResult = passed.json();
        assert!(result.ok);

        let connection: LlmConnection = get(&app, &cookie, id).await.json();
        assert_eq!(connection.status, ConnectionStatus::Ok);
        assert!(connection.last_error.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_404_on_an_explicit_model_keeps_the_status() {
        let mock = MockLlm::start(vec![Reply {
            status: 404,
            body: json!({ "model": "nope" }),
        }])
        .await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;

        let response = test(&app, &cookie, id, &json!({ "model": "nope" })).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        let result: LlmConnectionTestResult = response.json();
        assert!(!result.ok);
        assert_eq!(
            result.error.expect("the failure").class,
            TestErrorClass::Pause
        );

        // A 404 for a model typed in the test says nothing about the key.
        let connection: LlmConnection = get(&app, &cookie, id).await.json();
        assert_eq!(connection.status, ConnectionStatus::Ok);
        assert!(connection.last_error.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_500_is_retryable_and_keeps_the_status() {
        let mock = MockLlm::start(vec![Reply {
            status: 500,
            body: json!({ "error": { "message": "overloaded" } }),
        }])
        .await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;

        let response = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        let result: LlmConnectionTestResult = response.json();
        assert!(!result.ok);
        assert_eq!(
            result.error.expect("the failure").class,
            TestErrorClass::Retryable
        );

        let connection: LlmConnection = get(&app, &cookie, id).await.json();
        assert_eq!(connection.status, ConnectionStatus::Ok);
        assert!(connection.last_error.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_private_address_is_refused_without_the_setting() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        // The default config denies private-network egress.
        let (app, cookie) = app().await;
        let id = mocked(&app, &cookie, &mock).await;

        let response = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(response.status, 200, "{}", text(&response));
        let result: LlmConnectionTestResult = response.json();
        assert!(!result.ok);
        assert_eq!(
            result.error.expect("the failure").class,
            TestErrorClass::Permanent
        );
        assert!(mock.requests().is_empty(), "the mock saw no request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_uses_the_patched_key() {
        let mock = MockLlm::start(vec![
            completion(r#"{"value": true}"#),
            completion(r#"{"value": true}"#),
        ])
        .await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;

        let first = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(first.status, 200);
        assert!(first.json::<LlmConnectionTestResult>().ok);

        let patched = patch(
            &app,
            &cookie,
            id,
            &json!({ "api_key": "sk-second-key-0123456789" }),
        )
        .await;
        assert_eq!(patched.status, 200, "{}", text(&patched));

        let second = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(second.status, 200, "{}", text(&second));
        assert!(second.json::<LlmConnectionTestResult>().ok);
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            header_value(&requests[1].0, "authorization"),
            "Bearer sk-second-key-0123456789"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_after_delete_is_404() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;
        let deleted = delete(&app, &cookie, id).await;
        assert_eq!(deleted.status, 204);
        let response = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(response.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn member_cannot_test() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;
        let org = crate::meta::orgs::org_by_slug(app.state.meta.reader(), "acme")
            .await
            .unwrap()
            .expect("the seeded org");
        let (member_id, member) = signed_in(&app, "member@example.com").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;

        let response = test(&app, &member, id, &json!({})).await;
        assert_eq!(response.status, 403);
        assert!(mock.requests().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tests_are_rate_limited() {
        let mock = MockLlm::start(vec![completion(r#"{"value": true}"#)]).await;
        let (app, cookie) = egress_app().await;
        let id = mocked(&app, &cookie, &mock).await;

        for _ in 0..10 {
            let response = test(&app, &cookie, id, &json!({})).await;
            assert_eq!(response.status, 200, "{}", text(&response));
        }
        // The manual clock does not move: the 11th test in the minute is refused.
        let response = test(&app, &cookie, id, &json!({})).await;
        assert_eq!(response.status, 429);
    }
}
