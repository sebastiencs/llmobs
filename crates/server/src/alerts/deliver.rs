//! Delivery of alert notifications (D-K-7): one send, the retry policy, the due pass, pruning.

use std::time::Duration;

use futures::StreamExt;
use llmobs_eval::{EgressError, EgressRequest, SIGNATURE_HEADER, Secret, sign_payload};
use reqwest::Url;
use reqwest::header::{HeaderName, HeaderValue};
use uuid::Uuid;

use crate::alerts::channel::{ChannelConfig, open_config};
use crate::alerts::message::{self, MessageKind, Notice};
use crate::email::MailError;
use crate::http::AppState;
use crate::meta::MetaError;
use crate::meta::alerts::{Attempt, DueDelivery, due_deliveries, finish_delivery, prune};

/// Deliveries sent at once (D-K-7).
pub(crate) const CONCURRENCY: usize = 4;
/// Deliveries taken per pass.
pub(crate) const BATCH: i64 = 50;
/// Time allowed for one send (D-K-7).
pub(crate) const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Attempts before a delivery fails (D-K-7).
pub(crate) const MAX_ATTEMPTS: i64 = 8;
/// Response bodies read at most.
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
/// Events and finished deliveries kept (D-K-10).
pub(crate) const EVENT_RETENTION_NS: i64 = 90 * 86_400_000_000_000;
pub(crate) const DELIVERY_RETENTION_NS: i64 = 30 * 86_400_000_000_000;
/// Delivery id header, lower case. D46 renames it with the product.
pub(crate) const DELIVERY_HEADER: &str = "x-llmobs-delivery";

const INVALID_HEADER: &str = "invalid header";

/// Wait before attempt `attempts + 1`, after `attempts` failed: `min(30 s · 2^(attempts − 1), 1 h)`.
pub(crate) fn backoff_ns(attempts: i64) -> i64 {
    const BASE: i64 = 30_000_000_000;
    const CAP: i64 = 3_600_000_000_000;
    let shift = u32::try_from(attempts.saturating_sub(1).clamp(0, 20)).unwrap_or(20);
    BASE.saturating_mul(1_i64 << shift).min(CAP)
}

/// Why a send failed. The message never holds a URL, a header value, a secret or an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendError {
    pub(crate) retryable: bool,
    pub(crate) message: String,
}

fn permanent(message: &str) -> SendError {
    SendError {
        retryable: false,
        message: message.to_owned(),
    }
}

fn retryable(message: &str) -> SendError {
    SendError {
        retryable: true,
        message: message.to_owned(),
    }
}

/// Sends `notice` once to the channel `config`. `delivery_id` goes into `X-Llmobs-Delivery`.
pub(crate) async fn send_one(
    state: &AppState,
    config: &ChannelConfig,
    notice: &Notice,
    delivery_id: Uuid,
    now: i64,
) -> Result<(), SendError> {
    match config {
        ChannelConfig::Slack { url } => {
            // A `Value` always serializes.
            let body = serde_json::to_vec(&message::slack_body(notice)).unwrap_or_default();
            let headers = vec![(
                HeaderName::from_static("content-type"),
                HeaderValue::from_static("application/json"),
            )];
            post(state, url, headers, body).await
        }
        ChannelConfig::Webhook {
            url,
            secret,
            headers,
        } => {
            let body = message::webhook_body(notice);
            let signature = sign_payload(&Secret::new(secret.clone()), now / 1_000_000_000, &body)
                .ok_or_else(|| permanent("could not sign the payload"))?;
            let mut extra = vec![
                (
                    HeaderName::from_static("content-type"),
                    HeaderValue::from_static("application/json"),
                ),
                (
                    HeaderName::from_static(SIGNATURE_HEADER),
                    header_value(&signature)?,
                ),
                (
                    HeaderName::from_static(DELIVERY_HEADER),
                    header_value(&delivery_id.to_string())?,
                ),
            ];
            for (name, value) in headers {
                let name =
                    HeaderName::try_from(name.as_str()).map_err(|_| permanent(INVALID_HEADER))?;
                extra.push((name, header_value(value)?));
            }
            post(state, url, extra, body).await
        }
        ChannelConfig::Email { recipients } => send_email(state, recipients, notice).await,
    }
}

fn header_value(text: &str) -> Result<HeaderValue, SendError> {
    HeaderValue::from_str(text).map_err(|_| permanent(INVALID_HEADER))
}

/// One POST through the egress client. A 2xx is a success.
async fn post(
    state: &AppState,
    url: &str,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
) -> Result<(), SendError> {
    let url = Url::parse(url).map_err(|_| permanent("invalid URL"))?;
    let response = state
        .eval_runner
        .egress()
        .post(EgressRequest {
            url: &url,
            headers,
            body,
            timeout: SEND_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
        .await
        .map_err(|error| egress_error(&error))?;
    if response.status.is_success() {
        return Ok(());
    }
    let code = response.status.as_u16();
    Err(SendError {
        retryable: matches!(code, 408 | 429) || (500..=599).contains(&code),
        message: format!("the endpoint answered {code}"),
    })
}

fn egress_error(error: &EgressError) -> SendError {
    let (retry, message) = match error {
        EgressError::Timeout => (true, "the request timed out"),
        EgressError::Connect => (true, "could not connect"),
        EgressError::Request => (true, "the request failed"),
        EgressError::Denied { .. } => (
            false,
            "the address is not allowed (security.allow_private_network_egress)",
        ),
        EgressError::Scheme => (false, "unsupported URL scheme"),
        EgressError::TooLarge { .. } => (false, "the response was too large"),
    };
    SendError {
        retryable: retry,
        message: message.to_owned(),
    }
}

/// One email to each recipient, in order. SMTP connection, TLS and authentication failures
/// are retryable (an admin may fix the credentials within the retry span, D-K-7).
async fn send_email(
    state: &AppState,
    recipients: &[String],
    notice: &Notice,
) -> Result<(), SendError> {
    let Some(mailer) = state.mailer.as_ref() else {
        return Err(permanent("email is not configured"));
    };
    let email = message::email(notice);
    let sent = tokio::time::timeout(SEND_TIMEOUT, async {
        for recipient in recipients {
            mailer.send_now(recipient, &email).await?;
        }
        Ok::<(), MailError>(())
    })
    .await;
    match sent {
        Err(_elapsed) => Err(retryable("the request timed out")),
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(SendError {
            retryable: matches!(
                error,
                MailError::Connect(_) | MailError::Tls(_) | MailError::Auth(_)
            ),
            message: error.to_string(),
        }),
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeliverReport {
    pub(crate) sent: usize,
    pub(crate) retried: usize,
    pub(crate) failed: usize,
}

/// Sends every due delivery once, at most [`CONCURRENCY`] at a time.
///
/// # Errors
///
/// Returns [`MetaError`] only when the due deliveries cannot be read.
pub(crate) async fn deliver_once(state: &AppState, now: i64) -> Result<DeliverReport, MetaError> {
    let dues = due_deliveries(state.meta.reader(), now, BATCH).await?;
    let outcomes = futures::stream::iter(&dues)
        .map(|due| Box::pin(deliver_one(state, due, now)))
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    let mut report = DeliverReport::default();
    for outcome in outcomes.into_iter().flatten() {
        match outcome {
            Outcome::Sent => report.sent += 1,
            Outcome::Retried => report.retried += 1,
            Outcome::Failed => report.failed += 1,
        }
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    Retried,
    Failed,
}

/// Makes one attempt and records it. `None` when the final write fails; the row stays pending
/// and the next pass retries it.
async fn deliver_one(state: &AppState, due: &DueDelivery, now: i64) -> Option<Outcome> {
    let attempt = attempt_of(state, due, now).await;
    let (outcome, error) = match &attempt {
        Attempt::Sent => (Outcome::Sent, None),
        Attempt::Retry { error, .. } => (Outcome::Retried, Some(error.clone())),
        Attempt::Failed { error } => (Outcome::Failed, Some(error.clone())),
    };
    let id = due.id;
    let written = state
        .meta
        .write(move |conn| Box::pin(async move { finish_delivery(conn, id, attempt, now).await }))
        .await;
    if let Err(error) = written {
        tracing::error!(delivery_id = %due.id, error = %error, "alert delivery write failed");
        return None;
    }
    let label = match outcome {
        Outcome::Sent => "sent",
        Outcome::Retried => "retry",
        Outcome::Failed => "failed",
    };
    match &error {
        None => tracing::info!(
            delivery_id = %due.id,
            channel_id = %due.channel_id,
            kind = due.channel_kind.as_str(),
            attempt = due.attempts + 1,
            outcome = label,
            "alert delivery"
        ),
        Some(message) => tracing::info!(
            delivery_id = %due.id,
            channel_id = %due.channel_id,
            kind = due.channel_kind.as_str(),
            attempt = due.attempts + 1,
            outcome = label,
            error = %message,
            "alert delivery"
        ),
    }
    Some(outcome)
}

/// The result of one send, as the store records it.
async fn attempt_of(state: &AppState, due: &DueDelivery, now: i64) -> Attempt {
    let Ok(config) = open_config(&state.secrets, &due.channel_id, &due.config_enc) else {
        return Attempt::Failed {
            error: "the channel config could not be read".to_owned(),
        };
    };
    let notice = notice_of(state, due);
    match Box::pin(send_one(state, &config, &notice, due.id, now)).await {
        Ok(()) => Attempt::Sent,
        Err(error) if error.retryable && due.attempts + 1 < MAX_ATTEMPTS => Attempt::Retry {
            error: error.message,
            next_attempt_at: now.saturating_add(backoff_ns(due.attempts + 1)),
        },
        Err(error) => Attempt::Failed {
            error: error.message,
        },
    }
}

/// The message of a due delivery, built from its event snapshot.
fn notice_of(state: &AppState, due: &DueDelivery) -> Notice {
    let event = &due.event;
    let kind = match event.kind {
        crate::api::alerts::AlertEventKind::Fired => MessageKind::Fired,
        crate::api::alerts::AlertEventKind::Resolved => MessageKind::Resolved,
        crate::api::alerts::AlertEventKind::Reminder => MessageKind::Reminder,
    };
    let public_url = &state.config.public_url;
    Notice {
        kind,
        event_id: event.id,
        rule_id: event.rule_id,
        rule_name: event.details.rule_name.clone(),
        metric: event.details.metric,
        comparator: event.details.comparator,
        threshold: event.threshold,
        window: event.details.window,
        value: event.value,
        count: event.count,
        window_from: event.window_from,
        window_to: event.window_to,
        occurred_at: event.created_at,
        project_id: due.project_id,
        project_name: due.project_name.clone(),
        rule_url: message::rule_url(public_url, &due.org_slug, &due.project_slug, event.rule_id),
        traces_url: message::traces_url(
            public_url,
            &due.org_slug,
            &due.project_slug,
            event.window_from,
            event.window_to,
            event.details.filter.as_deref(),
        ),
    }
}

/// Deletes events older than 90 days and finished deliveries older than 30 days (D-K-10).
///
/// # Errors
///
/// Returns [`MetaError`] when the delete fails.
pub(crate) async fn prune_once(state: &AppState, now: i64) -> Result<(u64, u64), MetaError> {
    let events_before = now.saturating_sub(EVENT_RETENTION_NS);
    let deliveries_before = now.saturating_sub(DELIVERY_RETENTION_NS);
    let (events, deliveries) = state
        .meta
        .write(move |conn| {
            Box::pin(async move { prune(conn, events_before, deliveries_before).await })
        })
        .await?;
    if events > 0 || deliveries > 0 {
        tracing::info!(events, deliveries, "alert history pruned");
    }
    Ok((events, deliveries))
}

#[cfg(test)]
mod tests;
