//! Notification bodies and links (spec 07 § Channels, D-K-8).

use serde::Serialize;
use url::form_urlencoded::Serializer;
use uuid::Uuid;

use llmobs_core::ids::ProjectId;
use llmobs_core::names::PRODUCT;
use llmobs_core::time_serde;

use crate::api::alerts::{AlertMetric, AlertWindow, Comparator};
use crate::config::PublicUrl;
use crate::email::Email;

/// Why a message is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageKind {
    Fired,
    Resolved,
    Reminder,
    Test,
}

/// Everything a message shows. Built from an event (`deliver_once`) or a sample (test send).
/// Never log a `Notice`: it holds rule and project names and URLs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Notice {
    pub(crate) kind: MessageKind,
    /// The event id, or a fresh UUIDv7 for a test.
    pub(crate) event_id: Uuid,
    pub(crate) rule_id: Uuid,
    pub(crate) rule_name: String,
    pub(crate) metric: AlertMetric,
    pub(crate) comparator: Comparator,
    pub(crate) threshold: f64,
    pub(crate) window: AlertWindow,
    pub(crate) value: Option<f64>,
    pub(crate) count: Option<i64>,
    pub(crate) window_from: i64,
    pub(crate) window_to: i64,
    /// When the event was recorded.
    pub(crate) occurred_at: i64,
    pub(crate) project_id: ProjectId,
    pub(crate) project_name: String,
    pub(crate) rule_url: String,
    pub(crate) traces_url: String,
}

/// `<public_url>/<org_slug>/<project_slug>/alerts/<rule_id>`.
pub(crate) fn rule_url(
    public_url: &PublicUrl,
    org_slug: &str,
    project_slug: &str,
    rule_id: Uuid,
) -> String {
    public_url.join(&format!("/{org_slug}/{project_slug}/alerts/{rule_id}"))
}

/// `<public_url>/<org_slug>/<project_slug>/traces?from=<rfc3339>&to=<rfc3339>[&filter=<text>]`,
/// the query built with `url::form_urlencoded::Serializer` (so the filter is percent-encoded).
pub(crate) fn traces_url(
    public_url: &PublicUrl,
    org_slug: &str,
    project_slug: &str,
    from: i64,
    to: i64,
    filter: Option<&str>,
) -> String {
    let mut query = Serializer::new(String::new());
    query.append_pair("from", &rfc3339(from));
    query.append_pair("to", &rfc3339(to));
    if let Some(filter) = filter {
        query.append_pair("filter", filter);
    }
    public_url.join(&format!(
        "/{org_slug}/{project_slug}/traces?{}",
        query.finish()
    ))
}

fn rfc3339(ns: i64) -> String {
    let nanos = u32::try_from(ns.rem_euclid(1_000_000_000)).unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(ns.div_euclid(1_000_000_000), nanos)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        .unwrap_or_default()
}

/// How a value reads in a message: `12.5%` for `error_rate`, `$3.75` (2 decimals) for `cost`,
/// `820 ms` (0 decimals) for `latency_p95`, 3 decimals for `score` (`0.700`). `None` → `"no data"`.
pub(crate) fn value_text(metric: AlertMetric, value: Option<f64>) -> String {
    let Some(value) = value else {
        return "no data".to_owned();
    };
    match metric {
        AlertMetric::ErrorRate => format!("{:.1}%", value * 100.0),
        AlertMetric::Cost => format!("${value:.2}"),
        AlertMetric::LatencyP95 => format!("{value:.0} ms"),
        AlertMetric::Score => format!("{value:.3}"),
    }
}

/// The title of a message. The rule name is cut to 100 characters.
fn title(kind: MessageKind, rule_name: &str) -> String {
    let name: String = rule_name.chars().take(100).collect();
    match kind {
        MessageKind::Fired => format!("Firing: {name}"),
        MessageKind::Resolved => format!("Resolved: {name}"),
        MessageKind::Reminder => format!("Still firing: {name}"),
        MessageKind::Test => format!("Test notification from {PRODUCT}"),
    }
}

/// The state word of the email subject and body.
fn state_word(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Fired => "Firing",
        MessageKind::Resolved => "Resolved",
        MessageKind::Reminder => "Still firing",
        MessageKind::Test => "Test",
    }
}

/// Escapes text for Slack's `mrkdwn`. `&` goes first.
fn slack_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The Slack Block Kit body (D-K-8). The sender serializes it.
pub(crate) fn slack_body(notice: &Notice) -> serde_json::Value {
    let value = value_text(notice.metric, notice.value);
    let threshold = value_text(notice.metric, Some(notice.threshold));
    let symbol = slack_escape(notice.comparator.symbol());
    let project = slack_escape(&notice.project_name);
    let count = notice
        .count
        .map_or_else(|| "—".to_owned(), |count| count.to_string());
    let text = format!(
        "{} — {value} ({symbol} {threshold}) over {} in {project}",
        // Cut the raw name, then escape, so an entity is never split.
        slack_escape(&title(notice.kind, &notice.rule_name)),
        notice.window.as_str(),
    );
    serde_json::json!({
        "text": text,
        "blocks": [
            {"type": "header", "text": {"type": "plain_text", "text": title(notice.kind, &notice.rule_name)}},
            {"type": "section", "fields": [
                {"type": "mrkdwn", "text": format!("*Project*\n{project}")},
                {"type": "mrkdwn", "text": format!("*Metric*\n{}", notice.metric.as_str())},
                {"type": "mrkdwn", "text": format!("*Value*\n{value}")},
                {"type": "mrkdwn", "text": format!("*Threshold*\n{symbol} {threshold}")},
                {"type": "mrkdwn", "text": format!("*Window*\n{}", notice.window.as_str())},
                {"type": "mrkdwn", "text": format!("*Samples*\n{count}")},
            ]},
            {"type": "actions", "elements": [
                {"type": "button", "text": {"type": "plain_text", "text": "View rule"}, "url": notice.rule_url},
                {"type": "button", "text": {"type": "plain_text", "text": "View traces"}, "url": notice.traces_url},
            ]},
        ],
    })
}

/// The webhook body (spec 07 as amended by D-K-8).
#[derive(Debug, Serialize)]
pub(crate) struct WebhookPayload<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) event_id: Uuid,
    pub(crate) rule: WebhookRule<'a>,
    pub(crate) project: WebhookProject<'a>,
    pub(crate) value: Option<f64>,
    pub(crate) count: Option<i64>,
    pub(crate) threshold: f64,
    pub(crate) comparator: Comparator,
    pub(crate) window: AlertWindow,
    #[serde(with = "time_serde")]
    pub(crate) window_from: i64,
    #[serde(with = "time_serde")]
    pub(crate) window_to: i64,
    #[serde(with = "time_serde")]
    pub(crate) occurred_at: i64,
    pub(crate) url: &'a str,
    pub(crate) rule_url: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct WebhookRule<'a> {
    pub(crate) id: Uuid,
    pub(crate) name: &'a str,
    pub(crate) metric: AlertMetric,
}

#[derive(Debug, Serialize)]
pub(crate) struct WebhookProject<'a> {
    pub(crate) id: ProjectId,
    pub(crate) name: &'a str,
}

/// The signed webhook body. The payload holds strings, numbers and UUIDs only, so serializing
/// it cannot fail.
pub(crate) fn webhook_body(notice: &Notice) -> Vec<u8> {
    let kind = match notice.kind {
        MessageKind::Fired => "alert.fired",
        MessageKind::Resolved => "alert.resolved",
        MessageKind::Reminder => "alert.reminder",
        MessageKind::Test => "alert.test",
    };
    let payload = WebhookPayload {
        kind,
        event_id: notice.event_id,
        rule: WebhookRule {
            id: notice.rule_id,
            name: &notice.rule_name,
            metric: notice.metric,
        },
        project: WebhookProject {
            id: notice.project_id,
            name: &notice.project_name,
        },
        value: notice.value,
        count: notice.count,
        threshold: notice.threshold,
        comparator: notice.comparator,
        window: notice.window,
        window_from: notice.window_from,
        window_to: notice.window_to,
        occurred_at: notice.occurred_at,
        url: &notice.traces_url,
        rule_url: &notice.rule_url,
    };
    serde_json::to_vec(&payload).unwrap_or_default()
}

/// The email for `notice`.
pub(crate) fn email(notice: &Notice) -> Email {
    Email::Alert {
        state: state_word(notice.kind),
        rule_name: notice.rule_name.clone(),
        project_name: notice.project_name.clone(),
        value: value_text(notice.metric, notice.value),
        threshold: value_text(notice.metric, Some(notice.threshold)),
        window: notice.window.as_str(),
        rule_url: notice.rule_url.clone(),
        traces_url: notice.traces_url.clone(),
    }
}

/// A sample notice: error_rate, gt 0.05, 5m, value 0.125, count 48, window [now − 5 min, now).
#[cfg(test)]
pub(crate) fn sample_notice(kind: MessageKind, now: i64) -> Notice {
    Notice {
        kind,
        event_id: Uuid::now_v7(),
        rule_id: Uuid::now_v7(),
        rule_name: "High errors".to_owned(),
        metric: AlertMetric::ErrorRate,
        comparator: Comparator::Gt,
        threshold: 0.05,
        window: AlertWindow::M5,
        value: Some(0.125),
        count: Some(48),
        window_from: now - 300_000_000_000,
        window_to: now,
        occurred_at: now,
        project_id: "01900000-0000-7000-8000-000000000001"
            .parse()
            .expect("a project id"),
        project_name: "Support bot".to_owned(),
        rule_url: "https://obs.example.com/acme/support-bot/alerts/r".to_owned(),
        traces_url: "https://obs.example.com/acme/support-bot/traces?from=a&to=b".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn public() -> PublicUrl {
        serde_json::from_str("\"https://obs.example.com\"").expect("url")
    }

    #[test]
    fn links() {
        let id = Uuid::parse_str("01900000-0000-7000-8000-00000000000a").expect("uuid");
        assert_eq!(
            rule_url(&public(), "acme", "bot", id),
            format!("https://obs.example.com/acme/bot/alerts/{id}")
        );
        let url = traces_url(
            &public(),
            "acme",
            "bot",
            0,
            300_000_000_000,
            Some("environment = 'prod' & x"),
        );
        assert!(
            url.contains("filter=environment+%3D+%27prod%27+%26+x"),
            "{url}"
        );
        assert!(
            url.contains("from=1970-01-01T00%3A00%3A00.000000000Z"),
            "{url}"
        );
        assert!(
            url.contains("to=1970-01-01T00%3A05%3A00.000000000Z"),
            "{url}"
        );
    }

    #[test]
    fn value_texts() {
        assert_eq!(value_text(AlertMetric::ErrorRate, Some(0.125)), "12.5%");
        assert_eq!(value_text(AlertMetric::Cost, Some(3.754)), "$3.75");
        assert_eq!(value_text(AlertMetric::LatencyP95, Some(820.4)), "820 ms");
        assert_eq!(value_text(AlertMetric::Score, Some(0.7)), "0.700");
        assert_eq!(value_text(AlertMetric::Score, None), "no data");
    }

    #[test]
    fn slack_body_shape() {
        let mut notice = sample_notice(MessageKind::Fired, 1_000_000_000_000);
        notice.rule_name = "<b>&".to_owned();
        let body = slack_body(&notice);
        assert_eq!(body["blocks"][0]["text"]["text"], "Firing: <b>&");
        assert_eq!(
            body["text"],
            "Firing: &lt;b&gt;&amp; — 12.5% (&gt; 5.0%) over 5m in Support bot"
        );
        let fields = body["blocks"][1]["fields"].as_array().expect("fields");
        assert_eq!(fields.len(), 6);
        assert_eq!(fields[3]["text"], "*Threshold*\n&gt; 5.0%");
        assert_eq!(fields[5]["text"], "*Samples*\n48");
        let buttons = body["blocks"][2]["elements"].as_array().expect("buttons");
        assert_eq!(buttons[0]["url"], notice.rule_url.as_str());
        assert_eq!(buttons[1]["url"], notice.traces_url.as_str());

        notice.comparator = Comparator::Lte;
        let body = slack_body(&notice);
        let text = body["blocks"][1]["fields"][3]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("&lt;="), "{text}");
    }

    #[test]
    fn webhook_body_shape() {
        let mut notice = sample_notice(MessageKind::Resolved, 1_000_000_000_000);
        notice.value = None;
        notice.count = None;
        let body: Value = serde_json::from_slice(&webhook_body(&notice)).expect("json");
        assert_eq!(body["type"], "alert.resolved");
        assert_eq!(body["event_id"], notice.event_id.to_string());
        assert_eq!(body["rule"]["metric"], "error_rate");
        assert_eq!(body["comparator"], "gt");
        assert_eq!(body["window"], "5m");
        assert!(body.get("value").is_some_and(Value::is_null));
        assert!(body.get("count").is_some_and(Value::is_null));
        for key in ["window_from", "window_to", "occurred_at"] {
            assert!(
                body[key]
                    .as_str()
                    .is_some_and(|text| text.contains('T') && text.ends_with('Z')),
                "{key}"
            );
        }
    }
}
