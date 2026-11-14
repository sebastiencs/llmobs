//! Notification channels (spec 07 § Channels, D-K-5, D-K-6).

use axum::extract::{Extension, State};
use axum::http::{Method, StatusCode};
use axum::routing::{delete, get, patch, post};
use serde::Deserialize;
use uuid::Uuid;

use crate::alerts::channel::{ChannelConfig, open_config, seal_config};
use crate::alerts::deliver::send_one;
use crate::alerts::message::{self, MessageKind, Notice};
use crate::api::alerts::{
    AlertMetric, AlertWindow, ChannelDelivery, ChannelKind, ChannelTestResult, Comparator,
    CreateChannelRequest, DeliveryStatus, NotificationChannel, UpdateChannelRequest,
};
use crate::api::common::{FieldError, Page};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::alerts::{self, ChannelPatch, ChannelRow, LastDelivery, NewChannel};
use crate::meta::orgs;

/// Channel routes. Reads need `view`; writes need `manage_alerts` (D-K-9).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/channels",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_channels),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/channels",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: post(create_channel),
        },
        RouteDef {
            method: Method::PATCH,
            path: "/api/v1/projects/{project}/channels/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: patch(update_channel),
        },
        RouteDef {
            method: Method::DELETE,
            path: "/api/v1/projects/{project}/channels/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: delete(delete_channel),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/channels/{id}/test",
            access: Access::Project {
                perm: ProjectPerm::ManageAlerts,
                session_only: false,
            },
            documented: true,
            handler: post(test_channel),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct ChannelPath {
    id: Uuid,
}

/// Not paged: a project has at most 20 channels (D-K-5).
async fn list_channels(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
) -> Result<error::Json<Page<NotificationChannel>>, ApiError> {
    let project_id = access.project.id;
    let rows = alerts::list_channels(state.meta.reader(), project_id).await?;
    let last = alerts::last_deliveries(state.meta.reader(), project_id).await?;
    let data = rows
        .iter()
        .map(|row| channel_json(&state, row, last.get(&row.id)))
        .collect();
    Ok(error::Json(Page {
        data,
        next_cursor: None,
    }))
}

async fn create_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateChannelRequest>,
) -> Result<(StatusCode, error::Json<NotificationChannel>), ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let name = checked_name(&body.name)?;
    if body.kind == ChannelKind::Email && state.mailer.is_none() {
        return Err(field_error(
            "kind",
            "email channels need SMTP (smtp_url is not set)",
        ));
    }
    let config = config_from_create(&body)?;
    config.validate().map_err(|errors| ApiError::BadRequest {
        detail: None,
        errors,
    })?;
    let id = Uuid::now_v7();
    let config_enc = seal_config(&state.secrets, &id, &config)
        .map_err(|error| ApiError::Internal(Box::new(error)))?;
    let target_hint = config.target_hint();
    let project_id = access.project.id;
    let kind = body.kind;
    let now = state.clock.now_ns();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                alerts::insert_channel(
                    conn,
                    NewChannel {
                        id,
                        project_id,
                        kind,
                        name,
                        config_enc,
                        target_hint,
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    tracing::info!(
        target: "audit",
        channel_id = %row.id,
        project_id = %project_id,
        user_id = %user_id,
        kind = row.kind.as_str(),
        "alert channel created"
    );
    Ok((
        StatusCode::CREATED,
        error::Json(channel_json(&state, &row, None)),
    ))
}

async fn update_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(ChannelPath { id }): error::Path<ChannelPath>,
    error::Json(body): error::Json<UpdateChannelRequest>,
) -> Result<error::Json<NotificationChannel>, ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let project_id = access.project.id;
    let Some(current) = alerts::channel_by_id(state.meta.reader(), project_id, id).await? else {
        return Err(ApiError::NotFound);
    };
    let name = body.name.as_deref().map(checked_name).transpose()?;
    let config = if body.url.is_some()
        || body.secret.is_some()
        || body.headers.is_some()
        || body.recipients.is_some()
    {
        let stored = open_config(&state.secrets, &current.id, &current.config_enc)
            .map_err(|error| ApiError::Internal(Box::new(error)))?;
        let next = apply_update(stored, &body)?;
        next.validate().map_err(|errors| ApiError::BadRequest {
            detail: None,
            errors,
        })?;
        let sealed = seal_config(&state.secrets, &current.id, &next)
            .map_err(|error| ApiError::Internal(Box::new(error)))?;
        Some((sealed, next.target_hint()))
    } else {
        None
    };
    let patch = ChannelPatch { name, config };
    let now = state.clock.now_ns();
    let Some(row) = state
        .meta
        .write(move |conn| {
            Box::pin(async move { alerts::update_channel(conn, project_id, id, patch, now).await })
        })
        .await?
    else {
        return Err(ApiError::NotFound);
    };
    let last = alerts::last_deliveries(state.meta.reader(), project_id).await?;
    tracing::info!(
        target: "audit",
        channel_id = %row.id,
        project_id = %project_id,
        user_id = %user_id,
        kind = row.kind.as_str(),
        "alert channel updated"
    );
    Ok(error::Json(channel_json(&state, &row, last.get(&row.id))))
}

async fn delete_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(ChannelPath { id }): error::Path<ChannelPath>,
) -> Result<StatusCode, ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let project_id = access.project.id;
    let deleted = state
        .meta
        .write(move |conn| {
            Box::pin(async move { alerts::delete_channel(conn, project_id, id).await })
        })
        .await?;
    if !deleted {
        return Err(ApiError::NotFound);
    }
    tracing::info!(
        target: "audit",
        channel_id = %id,
        project_id = %project_id,
        user_id = %user_id,
        "alert channel deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Sends one sample message now and reports the result (D-K-7). Nothing is stored.
async fn test_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(ChannelPath { id }): error::Path<ChannelPath>,
) -> Result<error::Json<ChannelTestResult>, ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let project = &access.project;
    let Some(channel) = alerts::channel_by_id(state.meta.reader(), project.id, id).await? else {
        return Err(ApiError::NotFound);
    };
    let config = match open_config(&state.secrets, &channel.id, &channel.config_enc) {
        Ok(config) => config,
        Err(_) => {
            return Ok(error::Json(ChannelTestResult {
                ok: false,
                error: Some("the channel config could not be read".to_owned()),
            }));
        }
    };
    let org = orgs::org_by_id(state.meta.reader(), project.org_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let now = state.clock.now_ns();
    let window_from = now.saturating_sub(300_000_000_000);
    let public_url = &state.config.public_url;
    let notice = Notice {
        kind: MessageKind::Test,
        event_id: Uuid::now_v7(),
        rule_id: Uuid::nil(),
        rule_name: "Test alert".to_owned(),
        metric: AlertMetric::ErrorRate,
        comparator: Comparator::Gt,
        threshold: 0.05,
        window: AlertWindow::M5,
        value: Some(0.125),
        count: Some(48),
        window_from,
        window_to: now,
        occurred_at: now,
        project_id: project.id,
        project_name: project.name.clone(),
        rule_url: public_url.join(&format!("/{}/{}/alerts", org.slug, project.slug)),
        traces_url: message::traces_url(
            public_url,
            &org.slug,
            &project.slug,
            window_from,
            now,
            None,
        ),
    };
    let result = match Box::pin(send_one(&state, &config, &notice, Uuid::now_v7(), now)).await {
        Ok(()) => ChannelTestResult {
            ok: true,
            error: None,
        },
        Err(error) => ChannelTestResult {
            ok: false,
            error: Some(error.message),
        },
    };
    tracing::info!(
        target: "audit",
        channel_id = %id,
        project_id = %project.id,
        user_id = %user_id,
        ok = result.ok,
        "alert channel tested"
    );
    Ok(error::Json(result))
}

/// Whether `field` is a config field of `kind` (the request field names).
fn applies(kind: ChannelKind, field: &str) -> bool {
    matches!(
        (kind, field),
        (ChannelKind::Slack, "url")
            | (ChannelKind::Webhook, "url" | "secret" | "headers")
            | (ChannelKind::Email, "recipients")
    )
}

/// Whether a create of `kind` needs `field`. A webhook's headers are optional.
fn required(kind: ChannelKind, field: &str) -> bool {
    matches!(
        (kind, field),
        (ChannelKind::Slack, "url")
            | (ChannelKind::Webhook, "url" | "secret")
            | (ChannelKind::Email, "recipients")
    )
}

/// The config of a create body. Fields that do not belong to the kind are refused, and so is a
/// missing required field. Every problem is reported, in field order.
fn config_from_create(body: &CreateChannelRequest) -> Result<ChannelConfig, ApiError> {
    let kind = body.kind;
    let fields = [
        ("url", body.url.is_some()),
        ("secret", body.secret.is_some()),
        ("headers", body.headers.is_some()),
        ("recipients", body.recipients.is_some()),
    ];
    let mut errors = Vec::new();
    for (field, present) in fields {
        if !applies(kind, field) && present {
            errors.push(field_item(
                field,
                &format!("does not apply to {}", kind.as_str()),
            ));
        } else if required(kind, field) && !present {
            errors.push(field_item(field, "is required"));
        }
    }
    if !errors.is_empty() {
        return Err(bad_request(errors));
    }
    Ok(match kind {
        ChannelKind::Slack => ChannelConfig::Slack {
            url: body.url.clone().unwrap_or_default(),
        },
        ChannelKind::Webhook => ChannelConfig::Webhook {
            url: body.url.clone().unwrap_or_default(),
            secret: body.secret.clone().unwrap_or_default(),
            headers: body.headers.clone().unwrap_or_default(),
        },
        ChannelKind::Email => ChannelConfig::Email {
            recipients: body
                .recipients
                .iter()
                .flatten()
                .map(|entry| entry.trim().to_owned())
                .collect(),
        },
    })
}

/// The stored config with the fields of an update applied. A field of another kind is refused;
/// every such field is reported together.
fn apply_update(
    stored: ChannelConfig,
    body: &UpdateChannelRequest,
) -> Result<ChannelConfig, ApiError> {
    let kind = stored.kind();
    let fields = [
        ("url", body.url.is_some()),
        ("secret", body.secret.is_some()),
        ("headers", body.headers.is_some()),
        ("recipients", body.recipients.is_some()),
    ];
    let errors: Vec<FieldError> = fields
        .into_iter()
        .filter(|(field, present)| *present && !applies(kind, field))
        .map(|(field, _)| field_item(field, &format!("does not apply to {}", kind.as_str())))
        .collect();
    if !errors.is_empty() {
        return Err(bad_request(errors));
    }
    Ok(match stored {
        ChannelConfig::Slack { url } => ChannelConfig::Slack {
            url: body.url.clone().unwrap_or(url),
        },
        ChannelConfig::Webhook {
            url,
            secret,
            headers,
        } => ChannelConfig::Webhook {
            url: body.url.clone().unwrap_or(url),
            secret: body.secret.clone().unwrap_or(secret),
            headers: body.headers.clone().unwrap_or(headers),
        },
        ChannelConfig::Email { recipients } => ChannelConfig::Email {
            recipients: body.recipients.as_ref().map_or(recipients, |list| {
                list.iter().map(|entry| entry.trim().to_owned()).collect()
            }),
        },
    })
}

/// A trimmed channel name of 1 to 100 characters.
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

/// The response body of a channel. The config is opened only for the email recipients and the
/// header names; a config that does not open still lists (the list must answer).
fn channel_json(
    state: &AppState,
    row: &ChannelRow,
    last: Option<&LastDelivery>,
) -> NotificationChannel {
    let (recipients, header_names) = match open_config(&state.secrets, &row.id, &row.config_enc) {
        Ok(ChannelConfig::Email { recipients }) => (recipients, Vec::new()),
        Ok(ChannelConfig::Webhook { headers, .. }) => (Vec::new(), headers.into_keys().collect()),
        Ok(ChannelConfig::Slack { .. }) => (Vec::new(), Vec::new()),
        Err(_) => {
            tracing::warn!(channel_id = %row.id, "channel config does not open");
            (Vec::new(), Vec::new())
        }
    };
    let last_delivery = last.and_then(|last| {
        Some(ChannelDelivery {
            status: DeliveryStatus::parse(&last.status)?,
            at: last.at,
            error: last.error.clone(),
        })
    });
    NotificationChannel {
        id: row.id,
        name: row.name.clone(),
        kind: row.kind,
        target_hint: row.target_hint.clone(),
        available: row.kind != ChannelKind::Email || state.mailer.is_some(),
        recipients,
        header_names,
        last_delivery,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn field_item(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.to_owned(),
        position: None,
    }
}

fn bad_request(errors: Vec<FieldError>) -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors,
    }
}

fn field_error(field: &str, message: &str) -> ApiError {
    bad_request(vec![field_item(field, message)])
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};
    use serde_json::{Value, json};

    use super::*;
    use crate::api::alerts::{AlertCondition, AlertMetric, AlertWindow, Comparator};
    use crate::auth::roles::OrgRole;
    use crate::test_support::hook_receiver::HookReceiver;
    use crate::test_support::{Seeded, TestApp, TestResponse, assert_logs_clean};

    const SECRET: &str = "0123456789abcdef";
    const SLACK_URL: &str = "https://hooks.slack.com/services/T0/B0/XXXXabcd";
    const WEBHOOK_URL: &str = "https://hooks.example.com/x";

    fn channels(seeded: &Seeded) -> String {
        format!("/api/v1/projects/{}/channels", seeded.project.id)
    }

    fn pager() -> Value {
        json!({
            "name": "Pager",
            "kind": "webhook",
            "url": WEBHOOK_URL,
            "secret": SECRET,
            "headers": {"X-Team": "core"},
        })
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

    /// Creates a channel as the owner and expects `201`.
    async fn create_ok(app: &TestApp, seeded: &Seeded, body: Value) -> NotificationChannel {
        let response = send(
            app,
            Method::POST,
            &channels(seeded),
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

    #[tokio::test]
    async fn create_and_list_each_kind() {
        let app = TestApp::with_mailer().await;
        let seeded = app.seed().await;
        create_ok(
            &app,
            &seeded,
            json!({"name": "Team Slack", "kind": "slack", "url": SLACK_URL}),
        )
        .await;
        create_ok(&app, &seeded, pager()).await;
        create_ok(
            &app,
            &seeded,
            json!({"name": "Mail", "kind": "email", "recipients": ["oncall@example.com", " ada@example.com "]}),
        )
        .await;

        let list = send(&app, Method::GET, &channels(&seeded), &seeded.cookie, None).await;
        assert_eq!(list.status, 200);
        let text = String::from_utf8_lossy(&list.body).into_owned();
        for needle in ["XXXXabcd", "services/T0", SECRET, "\"core\""] {
            assert!(!text.contains(needle), "list contains {needle}");
        }
        let page: Page<NotificationChannel> = list.json();
        let names: Vec<&str> = page.data.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Mail", "Pager", "Team Slack"]);
        assert_eq!(
            page.data[0].recipients,
            ["oncall@example.com", "ada@example.com"]
        );
        assert_eq!(page.data[1].header_names, ["X-Team"]);
        assert_eq!(page.data[2].target_hint, "hooks.slack.com/…/abcd");
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn config_is_sealed_at_rest() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        create_ok(&app, &seeded, pager()).await;
        let sealed =
            sqlx::query_scalar::<_, Vec<u8>>("SELECT config_enc FROM notification_channels")
                .fetch_all(app.state.meta.reader())
                .await
                .unwrap();
        assert_eq!(sealed.len(), 1);
        assert!(
            !sealed[0]
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes())
        );
    }

    #[tokio::test]
    async fn fields_of_another_kind_are_refused() {
        let app = TestApp::with_mailer().await;
        let seeded = app.seed().await;
        let path = channels(&seeded);
        let slack = send(
            &app,
            Method::POST,
            &path,
            &seeded.cookie,
            Some(json!({"name": "S", "kind": "slack", "url": SLACK_URL, "secret": "x"})),
        )
        .await;
        assert_eq!(slack.status, 400);
        assert_eq!(
            problem_of(&slack),
            pairs(&[("secret", "does not apply to slack")])
        );

        let email = send(
            &app,
            Method::POST,
            &path,
            &seeded.cookie,
            Some(json!({"name": "M", "kind": "email"})),
        )
        .await;
        assert_eq!(email.status, 400);
        assert_eq!(problem_of(&email), pairs(&[("recipients", "is required")]));

        let short = send(
            &app,
            Method::POST,
            &path,
            &seeded.cookie,
            Some(json!({"name": "W", "kind": "webhook", "url": WEBHOOK_URL, "secret": "0123456789abcde"})),
        )
        .await;
        assert_eq!(short.status, 400);
        assert_eq!(
            problem_of(&short),
            pairs(&[("secret", "must be 16 to 256 characters")])
        );
    }

    #[tokio::test]
    async fn email_needs_smtp() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = send(
            &app,
            Method::POST,
            &channels(&seeded),
            &seeded.cookie,
            Some(json!({"name": "Mail", "kind": "email", "recipients": ["a@x.io"]})),
        )
        .await;
        assert_eq!(response.status, 400);
        assert_eq!(
            problem_of(&response),
            pairs(&[("kind", "email channels need SMTP (smtp_url is not set)")])
        );

        // A channel stored before SMTP was removed still lists, unavailable.
        let id = Uuid::now_v7();
        let project_id = seeded.project.id;
        let now = app.state.clock.now_ns();
        let sealed = seal_config(
            &app.state.secrets,
            &id,
            &ChannelConfig::Email {
                recipients: vec!["a@x.io".to_owned()],
            },
        )
        .unwrap();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    alerts::insert_channel(
                        conn,
                        NewChannel {
                            id,
                            project_id,
                            kind: ChannelKind::Email,
                            name: "Stored".to_owned(),
                            config_enc: sealed,
                            target_hint: "a@x.io".to_owned(),
                        },
                        now,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let list = send(&app, Method::GET, &channels(&seeded), &seeded.cookie, None).await;
        let page: Page<NotificationChannel> = list.json();
        assert_eq!(page.data.len(), 1);
        assert!(!page.data[0].available);
    }

    #[tokio::test]
    async fn names_are_unique() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        create_ok(&app, &seeded, pager()).await;
        let again = send(
            &app,
            Method::POST,
            &channels(&seeded),
            &seeded.cookie,
            Some(pager()),
        )
        .await;
        assert_eq!(again.status, 409);
    }

    #[tokio::test]
    async fn update_keeps_absent_fields() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let created = create_ok(&app, &seeded, pager()).await;
        let path = format!("{}/{}", channels(&seeded), created.id);

        let renamed = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"name": "Pager 2"})),
        )
        .await;
        assert_eq!(renamed.status, 200);
        let renamed: NotificationChannel = renamed.json();
        assert_eq!(renamed.name, "Pager 2");
        assert_eq!(renamed.header_names, ["X-Team"]);

        let cleared = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"headers": {}})),
        )
        .await;
        let cleared: NotificationChannel = cleared.json();
        assert!(cleared.header_names.is_empty());

        let refused = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"recipients": ["a@x.io"]})),
        )
        .await;
        assert_eq!(refused.status, 400);
        assert_eq!(
            problem_of(&refused),
            pairs(&[("recipients", "does not apply to webhook")])
        );

        let short = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"secret": ""})),
        )
        .await;
        assert_eq!(short.status, 400);
        assert_eq!(
            problem_of(&short),
            pairs(&[("secret", "must be 16 to 256 characters")])
        );

        let row = alerts::channel_by_id(app.state.meta.reader(), seeded.project.id, created.id)
            .await
            .unwrap()
            .unwrap();
        let ChannelConfig::Webhook { secret, .. } =
            open_config(&app.state.secrets, &row.id, &row.config_enc).unwrap()
        else {
            panic!("the channel is a webhook");
        };
        assert_eq!(secret, SECRET);
    }

    #[tokio::test]
    async fn update_and_delete_unknown_ids_are_not_found() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let unknown = "01900000-0000-7000-8000-000000009999";
        let path = format!("{}/{unknown}", channels(&seeded));
        let patched = send(
            &app,
            Method::PATCH,
            &path,
            &seeded.cookie,
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(patched.status, 404);
        let deleted = send(&app, Method::DELETE, &path, &seeded.cookie, None).await;
        assert_eq!(deleted.status, 404);
    }

    #[tokio::test]
    async fn delete_unlinks_rules() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let channel = create_ok(&app, &seeded, pager()).await;
        let condition = AlertCondition {
            metric: AlertMetric::ErrorRate,
            window: AlertWindow::M5,
            filter: None,
            comparator: Comparator::Gt,
            threshold: 0.5,
            min_count: Some(20),
            target: None,
            score_name: None,
            aggregation: None,
        };
        let rule = alerts::NewRule {
            project_id: seeded.project.id,
            name: "r".to_owned(),
            condition,
            channel_ids: vec![channel.id],
            enabled: true,
            cooldown_s: 3600,
            notify_while_firing: false,
            created_by: None,
        };
        let now = app.state.clock.now_ns();
        let rule_id = app
            .state
            .meta
            .write(move |conn| Box::pin(async move { alerts::insert_rule(conn, rule, now).await }))
            .await
            .unwrap()
            .id;
        let path = format!("{}/{}", channels(&seeded), channel.id);
        let deleted = send(&app, Method::DELETE, &path, &seeded.cookie, None).await;
        assert_eq!(deleted.status, 204);
        let loaded = alerts::rule_by_id(app.state.meta.reader(), seeded.project.id, rule_id)
            .await
            .unwrap()
            .unwrap();
        assert!(loaded.channel_ids.is_empty());
    }

    #[tokio::test]
    async fn roles_and_keys() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let channel = create_ok(&app, &seeded, pager()).await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let path = format!("{}/{}", channels(&seeded), channel.id);

        let listed = send(&app, Method::GET, &channels(&seeded), &viewer.cookie, None).await;
        assert_eq!(listed.status, 200);
        let created = send(
            &app,
            Method::POST,
            &channels(&seeded),
            &viewer.cookie,
            Some(json!({"name": "V", "kind": "slack", "url": SLACK_URL})),
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
            &channels(&seeded),
            &member.cookie,
            Some(json!({"name": "M", "kind": "slack", "url": SLACK_URL})),
        )
        .await;
        assert_eq!(by_member.status, 201);

        let bearer = format!("Bearer {}", seeded.read_key);
        let key_list = app
            .request(Method::GET, &channels(&seeded))
            .header(header::AUTHORIZATION.as_str(), &bearer)
            .send()
            .await;
        assert_eq!(key_list.status, 200);
        let key_create = app
            .request(Method::POST, &channels(&seeded))
            .header(header::AUTHORIZATION.as_str(), &bearer)
            .json(&json!({"name": "K", "kind": "slack", "url": SLACK_URL}))
            .send()
            .await;
        assert_eq!(key_create.status, 403);
    }

    #[tokio::test]
    async fn send_test_reports_the_result() {
        let app = TestApp::with_config(|config| {
            config.security.allow_private_network_egress = true;
        })
        .await;
        let seeded = app.seed().await;
        let accepting = HookReceiver::start(vec![200]).await;
        let refusing = HookReceiver::start(vec![500]).await;
        let good = create_ok(
            &app,
            &seeded,
            json!({"name": "Good", "kind": "webhook", "url": accepting.url("/hook"), "secret": SECRET}),
        )
        .await;
        let bad = create_ok(
            &app,
            &seeded,
            json!({"name": "Bad", "kind": "webhook", "url": refusing.url("/hook"), "secret": SECRET}),
        )
        .await;
        let test_path = |id: Uuid| format!("{}/{id}/test", channels(&seeded));

        let ok = send(
            &app,
            Method::POST,
            &test_path(good.id),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(ok.status, 200);
        assert_eq!(ok.json::<Value>(), json!({"ok": true}));
        let received = accepting.received();
        assert_eq!(received.len(), 1);
        assert!(String::from_utf8_lossy(&received[0].body).contains("\"type\":\"alert.test\""));

        let failed = send(&app, Method::POST, &test_path(bad.id), &seeded.cookie, None).await;
        assert_eq!(failed.status, 200);
        assert_eq!(
            failed.json::<Value>(),
            json!({"ok": false, "error": "the endpoint answered 500"})
        );

        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let denied = send(
            &app,
            Method::POST,
            &test_path(good.id),
            &viewer.cookie,
            None,
        )
        .await;
        assert_eq!(denied.status, 403);

        let unknown = send(
            &app,
            Method::POST,
            &test_path(Uuid::from_u128(9)),
            &seeded.cookie,
            None,
        )
        .await;
        assert_eq!(unknown.status, 404);
    }

    #[tokio::test]
    async fn logs_hold_no_secrets() {
        let app = TestApp::with_mailer().await;
        let seeded = app.seed().await;
        assert_logs_clean(
            &[SECRET, "hooks.example.com/x", "oncall@example.com"],
            async {
                let webhook = create_ok(&app, &seeded, pager()).await;
                let path = format!("{}/{}", channels(&seeded), webhook.id);
                let renamed = send(
                    &app,
                    Method::PATCH,
                    &path,
                    &seeded.cookie,
                    Some(json!({"name": "Pager 2"})),
                )
                .await;
                assert_eq!(renamed.status, 200);
                create_ok(
                    &app,
                    &seeded,
                    json!({"name": "Mail", "kind": "email", "recipients": ["oncall@example.com"]}),
                )
                .await;
            },
        )
        .await;
    }
}
