//! Alert rules and their event history (spec 09 § Evaluation & alerts, D-K-2, D-K-4, D-K-9,
//! D-K-14).

use std::collections::HashMap;

use axum::extract::{Extension, State};
use axum::http::{Method, StatusCode};
use axum::routing::{delete, get, patch, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use llmobs_core::ids::OrgId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::alerts::condition::normalize_condition;
use crate::alerts::quota;
use crate::api::alerts::{
    AlertCondition, AlertDelivery, AlertEvent, AlertRule, CreateAlertRuleRequest,
    UpdateAlertRuleRequest,
};
use crate::api::common::{FieldError, Page, PageParams};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::alerts::{self, NewRule, RulePatch, RuleRow};
use crate::routes::read::open_query;

const MIN_COOLDOWN_S: i64 = 300;
const MAX_COOLDOWN_S: i64 = 604_800;
const DEFAULT_COOLDOWN_S: i64 = 3_600;

/// Alert rule routes. Reads need `view`; writes need `manage_alerts` (D-K-9).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/alert-rules",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_rules),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/alert-rules",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: post(create_rule),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/alert-rules/{id}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_rule),
        },
        RouteDef {
            method: Method::PATCH,
            path: "/api/v1/projects/{project}/alert-rules/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: patch(update_rule),
        },
        RouteDef {
            method: Method::DELETE,
            path: "/api/v1/projects/{project}/alert-rules/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: delete(delete_rule),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/alert-rules/{id}/events",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_events),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct RulePath {
    id: Uuid,
}

/// Not paged: a project has at most 100 rules (D-K-5).
async fn list_rules(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
) -> Result<error::Json<Page<AlertRule>>, ApiError> {
    let rows = alerts::list_rules(state.meta.reader(), access.project.id).await?;
    Ok(error::Json(Page {
        data: rows.iter().map(rule_json).collect(),
        next_cursor: None,
    }))
}

async fn create_rule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateAlertRuleRequest>,
) -> Result<(StatusCode, error::Json<AlertRule>), ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let project_id = access.project.id;
    let org_id = access.project.org_id;
    let name = checked_name(&body.name)?;
    let condition = checked_condition(&state, project_id, body.condition).await?;
    let channel_ids = checked_channels(&state, project_id, &body.channel_ids).await?;
    let cooldown_s = checked_cooldown(body.cooldown_s.unwrap_or(DEFAULT_COOLDOWN_S))?;
    let enabled = body.enabled.unwrap_or(true);
    if enabled {
        enforce_quota(&state, org_id, quota::rule_limit(&state, org_id)).await?;
    }
    let notify_while_firing = body.notify_while_firing.unwrap_or(false);
    let now = state.clock.now_ns();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                alerts::insert_rule(
                    conn,
                    NewRule {
                        project_id,
                        name,
                        condition,
                        channel_ids,
                        enabled,
                        cooldown_s,
                        notify_while_firing,
                        created_by: Some(user_id),
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    tracing::info!(
        target: "audit",
        rule_id = %row.id,
        project_id = %project_id,
        user_id = %user_id,
        metric = row.condition.metric.as_str(),
        "alert rule created"
    );
    Ok((StatusCode::CREATED, error::Json(rule_json(&row))))
}

async fn get_rule(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(RulePath { id }): error::Path<RulePath>,
) -> Result<error::Json<AlertRule>, ApiError> {
    let Some(row) = alerts::rule_by_id(state.meta.reader(), access.project.id, id).await? else {
        return Err(ApiError::NotFound);
    };
    Ok(error::Json(rule_json(&row)))
}

async fn update_rule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(RulePath { id }): error::Path<RulePath>,
    error::Json(body): error::Json<UpdateAlertRuleRequest>,
) -> Result<error::Json<AlertRule>, ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let project_id = access.project.id;
    let org_id = access.project.org_id;
    let name = body.name.as_deref().map(checked_name).transpose()?;
    let condition = match body.condition {
        Some(condition) => Some(checked_condition(&state, project_id, condition).await?),
        None => None,
    };
    let channel_ids = match &body.channel_ids {
        Some(ids) => Some(checked_channels(&state, project_id, ids).await?),
        None => None,
    };
    let cooldown_s = body.cooldown_s.map(checked_cooldown).transpose()?;
    let Some(current) = alerts::rule_by_id(state.meta.reader(), project_id, id).await? else {
        return Err(ApiError::NotFound);
    };
    if body.enabled == Some(true) && !current.enabled {
        enforce_quota(&state, org_id, quota::rule_limit(&state, org_id)).await?;
    }
    let patch = RulePatch {
        name,
        condition,
        channel_ids,
        enabled: body.enabled,
        cooldown_s,
        notify_while_firing: body.notify_while_firing,
    };
    let now = state.clock.now_ns();
    let Some(row) = state
        .meta
        .write(move |conn| {
            Box::pin(async move { alerts::update_rule(conn, project_id, id, patch, now).await })
        })
        .await?
    else {
        return Err(ApiError::NotFound);
    };
    tracing::info!(
        target: "audit",
        rule_id = %row.id,
        project_id = %project_id,
        user_id = %user_id,
        "alert rule updated"
    );
    Ok(error::Json(rule_json(&row)))
}

async fn delete_rule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(RulePath { id }): error::Path<RulePath>,
) -> Result<StatusCode, ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let project_id = access.project.id;
    let deleted = state
        .meta
        .write(move |conn| Box::pin(async move { alerts::delete_rule(conn, project_id, id).await }))
        .await?;
    if !deleted {
        return Err(ApiError::NotFound);
    }
    tracing::info!(
        target: "audit",
        rule_id = %id,
        project_id = %project_id,
        user_id = %user_id,
        "alert rule deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Events of the rule, newest first, with their deliveries.
async fn list_events(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(RulePath { id }): error::Path<RulePath>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<AlertEvent>>, ApiError> {
    let reader = state.meta.reader();
    if alerts::rule_by_id(reader, access.project.id, id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    let before = params.cursor.as_deref().map(decode_cursor).transpose()?;
    let limit = usize::try_from(params.limit.get()).unwrap_or(usize::MAX);
    let fetch = i64::from(params.limit.get()) + 1;
    let mut rows = alerts::list_events(reader, id, before, fetch).await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = match (more, rows.last()) {
        (true, Some(last)) => Some(encode_cursor(last.created_at, last.id)?),
        _ => None,
    };
    let event_ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let mut by_event: HashMap<Uuid, Vec<AlertDelivery>> = HashMap::new();
    for delivery in alerts::deliveries_for_events(reader, &event_ids).await? {
        by_event
            .entry(delivery.event_id)
            .or_default()
            .push(AlertDelivery {
                id: delivery.id,
                channel_id: delivery.channel_id,
                channel_name: delivery.channel_name,
                channel_kind: delivery.channel_kind,
                status: delivery.status,
                attempts: delivery.attempts,
                last_error: delivery.last_error,
                updated_at: delivery.updated_at,
            });
    }
    let data = rows
        .into_iter()
        .map(|row| AlertEvent {
            id: row.id,
            rule_id: row.rule_id,
            kind: row.kind,
            value: row.value,
            count: row.count,
            threshold: row.threshold,
            comparator: row.details.comparator,
            metric: row.details.metric,
            window: row.details.window,
            window_from: row.window_from,
            window_to: row.window_to,
            created_at: row.created_at,
            deliveries: by_event.remove(&row.id).unwrap_or_default(),
        })
        .collect();
    Ok(error::Json(Page { data, next_cursor }))
}

/// A trimmed rule name of 1 to 100 characters.
fn checked_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(field_error("name", "is required"));
    }
    if name.chars().count() > 100 {
        return Err(field_error("name", "at most 100 characters"));
    }
    Ok(name.to_owned())
}

fn checked_cooldown(value: i64) -> Result<i64, ApiError> {
    if (MIN_COOLDOWN_S..=MAX_COOLDOWN_S).contains(&value) {
        Ok(value)
    } else {
        Err(field_error("cooldown_s", "must be between 300 and 604800"))
    }
}

/// The channel ids of the project, sorted. A rule has at most 10, none twice, all its own.
async fn checked_channels(
    state: &AppState,
    project: llmobs_core::ids::ProjectId,
    ids: &[Uuid],
) -> Result<Vec<Uuid>, ApiError> {
    if ids.len() > alerts::MAX_RULE_CHANNELS {
        return Err(field_error("channel_ids", "at most 10 channels"));
    }
    let mut sorted = ids.to_vec();
    sorted.sort();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(field_error("channel_ids", "duplicate channel"));
    }
    let known = alerts::list_channels(state.meta.reader(), project).await?;
    for id in &sorted {
        if !known.iter().any(|channel| channel.id == *id) {
            return Err(field_error("channel_ids", "unknown channel"));
        }
    }
    Ok(sorted)
}

/// The normalized condition. A filter is checked by the query layer, so a bad one is
/// `400 invalid_filter` on the `filter` field.
async fn checked_condition(
    state: &AppState,
    project: llmobs_core::ids::ProjectId,
    condition: AlertCondition,
) -> Result<AlertCondition, ApiError> {
    let condition = normalize_condition(condition).map_err(|errors| ApiError::BadRequest {
        detail: None,
        errors,
    })?;
    if let Some(text) = &condition.filter {
        let ctx = open_query(state, project, None).await?;
        llmobs_storage::check_alert_filter(&ctx, text)?;
    }
    Ok(condition)
}

/// Refuses one more enabled rule in `org` when `limit` is reached (D-K-14).
async fn enforce_quota(state: &AppState, org: OrgId, limit: Option<u64>) -> Result<(), ApiError> {
    if limit.is_none() {
        return Ok(());
    }
    let enabled = alerts::count_enabled_org_rules(state.meta.reader(), org).await?;
    quota::check_rule_quota(enabled, limit)
}

fn rule_json(row: &RuleRow) -> AlertRule {
    AlertRule {
        id: row.id,
        name: row.name.clone(),
        enabled: row.enabled,
        condition: row.condition.clone(),
        channel_ids: row.channel_ids.clone(),
        cooldown_s: row.cooldown_s,
        notify_while_firing: row.notify_while_firing,
        state: row.state,
        last_value: row.last_value,
        last_count: row.last_count,
        last_evaluated_at: row.last_evaluated_at,
        last_fired_at: row.last_fired_at,
        last_error: row.last_error.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// The cursor of an event page: the `(created_at, id)` of the last event of the page.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventCursor {
    created_at: i64,
    id: Uuid,
}

fn encode_cursor(created_at: i64, id: Uuid) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&EventCursor { created_at, id })
        .map_err(|error| ApiError::Internal(Box::new(error)))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(text: &str) -> Result<(i64, Uuid), ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| field_error("cursor", "invalid cursor"))?;
    let cursor: EventCursor =
        serde_json::from_slice(&bytes).map_err(|_| field_error("cursor", "invalid cursor"))?;
    Ok((cursor.created_at, cursor.id))
}

fn field_error(field: &str, message: &str) -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors: vec![FieldError {
            field: field.to_owned(),
            message: message.to_owned(),
            position: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};
    use serde_json::{Value, json};

    use super::*;
    use crate::api::alerts::{AlertState, ChannelKind};
    use crate::auth::roles::OrgRole;
    use crate::meta::alerts::NewChannel;
    use crate::test_support::{Seeded, TestApp, TestResponse, assert_logs_clean};

    fn rules(seeded: &Seeded) -> String {
        format!("/api/v1/projects/{}/alert-rules", seeded.project.id)
    }

    /// Sends one request with a session cookie.
    async fn send(
        app: &TestApp,
        method: Method,
        path: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> TestResponse {
        let mut request = app
            .request(method, path)
            .header(header::COOKIE.as_str(), cookie);
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await
    }

    /// Creates a rule as the owner and expects `201`.
    async fn create_ok(app: &TestApp, seeded: &Seeded, body: Value) -> AlertRule {
        let response = send(
            app,
            Method::POST,
            &rules(seeded),
            &seeded.cookie,
            Some(body),
        )
        .await;
        assert_eq!(
            response.status,
            201,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    fn rule_body(name: &str) -> Value {
        json!({
            "name": name,
            "condition": {"metric": "error_rate", "window": "5m", "comparator": "gt", "threshold": 0.05},
        })
    }

    /// `base` with the fields of `extra` added or replaced.
    fn with(mut base: Value, extra: Value) -> Value {
        if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
            for (key, value) in extra {
                base.insert(key, value);
            }
        }
        base
    }

    /// The `(field, message)` pairs of a problem response.
    fn problem_of(response: &TestResponse) -> Vec<(String, String)> {
        response
            .problem()
            .errors
            .into_iter()
            .map(|error| (error.field, error.message))
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(field, message)| ((*field).to_owned(), (*message).to_owned()))
            .collect()
    }

    async fn insert_channel(
        app: &TestApp,
        project: llmobs_core::ids::ProjectId,
        name: &str,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let name = name.to_owned();
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    alerts::insert_channel(
                        conn,
                        NewChannel {
                            id,
                            project_id: project,
                            kind: ChannelKind::Webhook,
                            name,
                            config_enc: vec![1, 2, 3],
                            target_hint: "x".to_owned(),
                        },
                        now,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        id
    }

    /// Inserts an event of `rule` by SQL and returns its id.
    async fn insert_event(app: &TestApp, rule: Uuid, created_at: i64) -> Uuid {
        let id = Uuid::now_v7();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO alert_events (id, rule_id, kind, threshold, window_from, window_to, details, created_at) \
                         VALUES (?, ?, 'fired', 0.1, 0, 1, '{\"rule_name\":\"r\",\"metric\":\"error_rate\",\"comparator\":\"gt\",\"window\":\"5m\"}', ?)",
                    )
                    .bind(id.to_string())
                    .bind(rule.to_string())
                    .bind(created_at)
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        id
    }

    /// Puts a rule into `firing` with a value, the way the evaluator will.
    async fn set_firing(app: &TestApp, id: Uuid) {
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE alert_rules SET state = 'firing', last_value = 0.9 WHERE id = ?",
                    )
                    .bind(id.to_string())
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_fills_defaults() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, rule_body(" High errors ")).await;
        assert_eq!(created.name, "High errors");
        assert_eq!(created.condition.min_count, Some(20));
        assert!(created.enabled);
        assert_eq!(created.cooldown_s, 3600);
        assert!(!created.notify_while_firing);
        assert_eq!(created.state, AlertState::Ok);
        assert!(created.channel_ids.is_empty());

        let path = format!("{}/{}", rules(&seeded), created.id);
        let loaded = send(&app, Method::GET, &path, &seeded.cookie, None).await;
        assert_eq!(loaded.status, 200);
        let loaded: AlertRule = loaded.json();
        assert_eq!(loaded, created);
    }

    #[tokio::test]
    async fn invalid_condition_is_400_with_fields() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let refused = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(json!({
                "name": "Costly",
                "condition": {"metric": "cost", "window": "1h", "comparator": "gt", "threshold": -1, "min_count": 5},
            })),
        )
        .await;
        assert_eq!(refused.status, 400);
        let fields: Vec<String> = problem_of(&refused)
            .into_iter()
            .map(|(field, _)| field)
            .collect();
        assert_eq!(fields, ["condition.threshold", "condition.min_count"]);

        let blank = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(json!({
                "name": "Blank",
                "condition": {"metric": "error_rate", "window": "5m", "filter": "  ", "comparator": "gt", "threshold": 0.05},
            })),
        )
        .await;
        assert_eq!(blank.status, 201);
        let blank: AlertRule = blank.json();
        assert!(blank.condition.filter.is_none());
    }

    #[tokio::test]
    async fn invalid_filter_is_invalid_filter() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let broken = json!({
            "name": "Broken",
            "condition": {"metric": "error_rate", "window": "5m", "filter": "error_count >", "comparator": "gt", "threshold": 0.05},
        });
        let refused = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(broken),
        )
        .await;
        assert_eq!(refused.status, 400);
        let problem = refused.problem();
        assert_eq!(problem.code, crate::api::common::ProblemCode::InvalidFilter);
        assert_eq!(problem.errors[0].field, "filter");
        assert!(problem.errors[0].position.is_some());

        let created = create_ok(
            &app,
            &seeded,
            json!({
                "name": "Errors",
                "condition": {"metric": "error_rate", "window": "5m", "filter": "error_count > 0", "comparator": "gt", "threshold": 0.05},
            }),
        )
        .await;
        let path = format!("{}/{}", rules(&seeded), created.id);
        let patched = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({
                "condition": {"metric": "error_rate", "window": "5m", "filter": "error_count >", "comparator": "gt", "threshold": 0.05},
            })),
        )
        .await;
        assert_eq!(patched.status, 400);
        assert_eq!(
            patched.problem().code,
            crate::api::common::ProblemCode::InvalidFilter
        );
    }

    #[tokio::test]
    async fn unknown_fields_are_refused() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(json!({
                "name": "Grouped",
                "condition": {"metric": "error_rate", "window": "5m", "comparator": "gt", "threshold": 0.05, "group_by": "model"},
            })),
        )
        .await;
        assert_eq!(response.status, 400);
    }

    #[tokio::test]
    async fn channels_are_checked() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (other, _) = app.add_project(&seeded, "other", None).await;
        let foreign = insert_channel(&app, other.id, "Foreign").await;
        let own_a = insert_channel(&app, seeded.project.id, "A").await;
        let own_b = insert_channel(&app, seeded.project.id, "B").await;

        let unknown = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(with(rule_body("R"), json!({"channel_ids": [foreign]}))),
        )
        .await;
        assert_eq!(unknown.status, 400);
        assert_eq!(
            problem_of(&unknown),
            pairs(&[("channel_ids", "unknown channel")])
        );

        let twice = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(with(rule_body("R"), json!({"channel_ids": [own_a, own_a]}))),
        )
        .await;
        assert_eq!(twice.status, 400);
        assert_eq!(
            problem_of(&twice),
            pairs(&[("channel_ids", "duplicate channel")])
        );

        let eleven: Vec<Uuid> = (0..11).map(|_| Uuid::now_v7()).collect();
        let too_many = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(with(rule_body("R"), json!({"channel_ids": eleven}))),
        )
        .await;
        assert_eq!(too_many.status, 400);
        assert_eq!(
            problem_of(&too_many),
            pairs(&[("channel_ids", "at most 10 channels")])
        );

        let created = create_ok(
            &app,
            &seeded,
            with(rule_body("R"), json!({"channel_ids": [own_b, own_a]})),
        )
        .await;
        let mut expected = vec![own_a, own_b];
        expected.sort();
        assert_eq!(created.channel_ids, expected);
    }

    #[tokio::test]
    async fn cooldown_range() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let short = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(with(rule_body("R"), json!({"cooldown_s": 299}))),
        )
        .await;
        assert_eq!(short.status, 400);
        assert_eq!(
            problem_of(&short),
            pairs(&[("cooldown_s", "must be between 300 and 604800")])
        );
        let ok = create_ok(
            &app,
            &seeded,
            with(rule_body("R"), json!({"cooldown_s": 300})),
        )
        .await;
        assert_eq!(ok.cooldown_s, 300);
    }

    #[tokio::test]
    async fn update_resets_state_on_condition_change() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, rule_body("Rule")).await;
        let path = format!("{}/{}", rules(&seeded), created.id);
        set_firing(&app, created.id).await;

        let renamed = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(renamed.status, 200);
        let renamed: AlertRule = renamed.json();
        assert_eq!(renamed.state, AlertState::Firing);
        assert_eq!(renamed.last_value, Some(0.9));

        let empty = send(&app, Method::PATCH, &path, &seeded.cookie, Some(json!({}))).await;
        assert_eq!(empty.status, 200);
        let empty: AlertRule = empty.json();
        assert_eq!(empty, renamed);

        let same = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"condition": renamed.condition})),
        )
        .await;
        let same: AlertRule = same.json();
        assert_eq!(same.state, AlertState::Firing);

        let changed = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"condition": {"metric": "error_rate", "window": "5m", "comparator": "gt", "threshold": 0.1}})),
        )
        .await;
        let changed: AlertRule = changed.json();
        assert_eq!(changed.state, AlertState::Ok);
        assert!(changed.last_value.is_none());

        set_firing(&app, created.id).await;
        let disabled = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"enabled": false})),
        )
        .await;
        let disabled: AlertRule = disabled.json();
        assert_eq!(disabled.state, AlertState::Ok);
        assert!(!disabled.enabled);

        let unlinked = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"channel_ids": []})),
        )
        .await;
        let unlinked: AlertRule = unlinked.json();
        assert!(unlinked.channel_ids.is_empty());

        let short = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"cooldown_s": 10})),
        )
        .await;
        assert_eq!(short.status, 400);
    }

    #[tokio::test]
    async fn names_are_unique() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        create_ok(&app, &seeded, rule_body("Taken")).await;
        let again = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &seeded.cookie,
            Some(rule_body("Taken")),
        )
        .await;
        assert_eq!(again.status, 409);

        let other = create_ok(&app, &seeded, rule_body("Other")).await;
        let path = format!("{}/{}", rules(&seeded), other.id);
        let renamed = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"name": "Taken"})),
        )
        .await;
        assert_eq!(renamed.status, 409);
    }

    #[tokio::test]
    async fn delete_and_404s() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, rule_body("Gone")).await;
        let path = format!("{}/{}", rules(&seeded), created.id);

        let deleted = send(&app, Method::DELETE, &path, &seeded.cookie, None).await;
        assert_eq!(deleted.status, 204);
        for method in [Method::GET, Method::DELETE] {
            let gone = send(&app, method, &path, &seeded.cookie, None).await;
            assert_eq!(gone.status, 404);
        }
        let gone = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(gone.status, 404);

        let malformed = send(
            &app,
            Method::GET,
            &format!("{}/not-a-uuid", rules(&seeded)),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(malformed.status, 400);

        let (other, _) = app.add_project(&seeded, "other", None).await;
        let now = app.state.clock.now_ns();
        let foreign = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    alerts::insert_rule(
                        conn,
                        NewRule {
                            project_id: other.id,
                            name: "Foreign".to_owned(),
                            condition: serde_json::from_value(json!({
                                "metric": "error_rate", "window": "5m", "comparator": "gt", "threshold": 0.5, "min_count": 20
                            }))
                            .unwrap(),
                            channel_ids: Vec::new(),
                            enabled: true,
                            cooldown_s: 3600,
                            notify_while_firing: false,
                            created_by: None,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let foreign_path = format!("{}/{}", rules(&seeded), foreign.id);
        let get = send(&app, Method::GET, &foreign_path, &seeded.cookie, None).await;
        assert_eq!(get.status, 404);
        let patch = send(
            &app,
            Method::PATCH,
            &foreign_path,
            &seeded.cookie,
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(patch.status, 404);
        let delete = send(&app, Method::DELETE, &foreign_path, &seeded.cookie, None).await;
        assert_eq!(delete.status, 404);
    }

    #[tokio::test]
    async fn events_are_paged_with_deliveries() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let rule = create_ok(&app, &seeded, rule_body("Events")).await;
        let slack = insert_channel(&app, seeded.project.id, "Slack").await;
        let mail = insert_channel(&app, seeded.project.id, "Mail").await;
        let oldest = insert_event(&app, rule.id, 1).await;
        let middle = insert_event(&app, rule.id, 2).await;
        let newest = insert_event(&app, rule.id, 3).await;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    for (channel, created_at) in [(slack, 3), (mail, 4)] {
                        sqlx::query(
                            "INSERT INTO notification_deliveries (id, event_id, channel_id, status, \
                             attempts, created_at, updated_at) VALUES (?, ?, ?, 'sent', 1, ?, ?)",
                        )
                        .bind(Uuid::now_v7().to_string())
                        .bind(newest.to_string())
                        .bind(channel.to_string())
                        .bind(created_at)
                        .bind(created_at)
                        .execute(&mut *conn)
                        .await?;
                    }
                    Ok(())
                })
            })
            .await
            .unwrap();

        let base = format!("{}/{}/events", rules(&seeded), rule.id);
        let first = send(
            &app,
            Method::GET,
            &format!("{base}?limit=2"),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(first.status, 200);
        let first: Page<AlertEvent> = first.json();
        let ids: Vec<Uuid> = first.data.iter().map(|event| event.id).collect();
        assert_eq!(ids, [newest, middle]);
        assert_eq!(first.data[0].deliveries.len(), 2);
        let names: Vec<&str> = first.data[0]
            .deliveries
            .iter()
            .map(|delivery| delivery.channel_name.as_str())
            .collect();
        assert_eq!(names, ["Slack", "Mail"]);
        let cursor = first.next_cursor.expect("a second page exists");

        let second = send(
            &app,
            Method::GET,
            &format!("{base}?limit=2&cursor={cursor}"),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(second.status, 200);
        let second: Page<AlertEvent> = second.json();
        let ids: Vec<Uuid> = second.data.iter().map(|event| event.id).collect();
        assert_eq!(ids, [oldest]);
        assert!(second.next_cursor.is_none());

        let garbage = send(
            &app,
            Method::GET,
            &format!("{base}?cursor=garbage"),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(garbage.status, 400);
        assert_eq!(problem_of(&garbage), pairs(&[("cursor", "invalid cursor")]));

        let zero = send(
            &app,
            Method::GET,
            &format!("{base}?limit=0"),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(zero.status, 400);

        let (other, _) = app.add_project(&seeded, "other", None).await;
        let now = app.state.clock.now_ns();
        let foreign = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    alerts::insert_rule(
                        conn,
                        NewRule {
                            project_id: other.id,
                            name: "Foreign".to_owned(),
                            condition: serde_json::from_value(json!({
                                "metric": "error_rate", "window": "5m", "comparator": "gt", "threshold": 0.5, "min_count": 20
                            }))
                            .unwrap(),
                            channel_ids: Vec::new(),
                            enabled: true,
                            cooldown_s: 3600,
                            notify_while_firing: false,
                            created_by: None,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let foreign_events = send(
            &app,
            Method::GET,
            &format!("{}/{}/events", rules(&seeded), foreign.id),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(foreign_events.status, 404);
    }

    #[tokio::test]
    async fn no_plan_limit_by_default() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let first = create_ok(&app, &seeded, rule_body("First")).await;
        create_ok(&app, &seeded, rule_body("Second")).await;
        let path = format!("{}/{}", rules(&seeded), first.id);
        let disabled = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"enabled": false})),
        )
        .await;
        assert_eq!(disabled.status, 200);
        let reenabled = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"enabled": true})),
        )
        .await;
        assert_eq!(reenabled.status, 200);
    }

    #[tokio::test]
    async fn enforce_quota_counts_enabled_rules() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let rule = create_ok(&app, &seeded, rule_body("Only")).await;

        let refused = enforce_quota(&app.state, seeded.org_id, Some(1)).await;
        let Err(error) = refused else {
            panic!("one enabled rule reaches a limit of one");
        };
        let problem = error.problem();
        assert_eq!(problem.status, 403);
        assert_eq!(
            serde_json::to_value(problem.code).unwrap(),
            json!("plan_limit")
        );

        let path = format!("{}/{}", rules(&seeded), rule.id);
        let disabled = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"enabled": false})),
        )
        .await;
        assert_eq!(disabled.status, 200);
        assert!(
            enforce_quota(&app.state, seeded.org_id, Some(1))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn audit_logs_hold_no_names() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        assert_logs_clean(&["Secret name", "x-secret"], async {
            create_ok(
                &app,
                &seeded,
                json!({
                    "name": "Secret name",
                    "condition": {"metric": "error_rate", "window": "5m", "filter": "environment = 'x-secret'", "comparator": "gt", "threshold": 0.05},
                }),
            )
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn roles() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let rule = create_ok(&app, &seeded, rule_body("Roles")).await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let path = format!("{}/{}", rules(&seeded), rule.id);
        let events = format!("{path}/events");

        for listed in [rules(&seeded), path.clone(), events.clone()] {
            let response = send(&app, Method::GET, &listed, &viewer.cookie, None).await;
            assert_eq!(response.status, 200, "{listed}");
        }
        let created = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &viewer.cookie,
            Some(rule_body("V")),
        )
        .await;
        assert_eq!(created.status, 403);
        let patched = send(
            &app,
            Method::PATCH,
            &path,
            &viewer.cookie,
            Some(json!({"name": "V"})),
        )
        .await;
        assert_eq!(patched.status, 403);
        let deleted = send(&app, Method::DELETE, &path, &viewer.cookie, None).await;
        assert_eq!(deleted.status, 403);

        let by_member = send(
            &app,
            Method::POST,
            &rules(&seeded),
            &member.cookie,
            Some(rule_body("M")),
        )
        .await;
        assert_eq!(by_member.status, 201);

        let bearer = format!("Bearer {}", seeded.read_key);
        let key_list = app
            .request(Method::GET, &rules(&seeded))
            .header(header::AUTHORIZATION.as_str(), &bearer)
            .send()
            .await;
        assert_eq!(key_list.status, 200);
        let key_create = app
            .request(Method::POST, &rules(&seeded))
            .header(header::AUTHORIZATION.as_str(), &bearer)
            .json(&rule_body("K"))
            .send()
            .await;
        assert_eq!(key_create.status, 403);
    }
}
