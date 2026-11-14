# K8 — Messages, delivery with retries, "Send test", pruning

**Commit subject:** `server: deliver alert notifications`

Adds:
- `alerts::message`: the Slack Block Kit body, the signed webhook JSON, the `alert` email and the two links (D-K-8);
- `alerts::deliver`: `send_one` (one attempt through the egress client or the mailer), `deliver_once(state, now)` (the due deliveries, with retries, D-K-7) and `prune_once(state, now)` (D-K-10);
- `POST /api/v1/projects/{project}/channels/{id}/test` ("Send test").

No background task yet (K9).

## Read first

- README § Design → Delivery, D-K-6, D-K-7, D-K-8, D-K-10, § Conventions → Privacy.
- K2 (`llmobs_eval::{EgressClient, EgressRequest, EgressResponse, EgressError, SIGNATURE_HEADER, sign_payload, Secret}`, `EvalRunner::egress`), K3 (`ChannelConfig`, `Comparator::symbol`, `AlertWindow::as_str`, `AlertMetric::as_str`), K4/K6/K7 (`meta::alerts`: `EventDetails`, `DeliveryRow`, `open_config`), K5 (`routes/channels.rs`).
- `crates/server/src/email/mod.rs` (`Email`, `TEMPLATES`, `render`, `subject`, `template_name`, `send_now`, `MailError`, the test `templates_render`), `crates/server/src/email/templates/test.html` and `invite.txt` (layout to copy).
- `crates/server/src/config.rs`: `PublicUrl::join`; `state.config.public_url`.
- `crates/server/src/http/mod.rs`: `AppInner.eval_runner` (I M1), `AppInner.mailer`, `AppInner.secrets`.
- `crates/server/src/test_support/mod.rs`: `TestApp::with_config`, `with_mailer`, `with_mailer_config`, `sent()`, `Mailer::fail_next`.

## Files

| File | Change |
|---|---|
| `crates/server/src/alerts/message.rs` | new |
| `crates/server/src/alerts/deliver.rs` | new |
| `crates/server/src/alerts/mod.rs` | `pub(crate) mod deliver;` `pub(crate) mod message;` |
| `crates/server/src/meta/alerts.rs` | `DueDelivery`, `due_deliveries`, `Attempt`, `finish_delivery`, `prune` |
| `crates/server/src/email/mod.rs` | `Email::Alert`, two templates |
| `crates/server/src/email/templates/alert.txt`, `alert.html` | new |
| `crates/server/tests/snapshots/alert.snap` | new (insta) |
| `crates/server/src/routes/channels.rs` | the test route |
| `crates/server/src/test_support/hook_receiver.rs`, `test_support/mod.rs` | a small HTTP receiver for tests |
| `crates/server/src/api/alerts.rs` | `ChannelTestResult` |
| OpenAPI: `api/openapi.rs`, `tests/openapi.rs`, `openapi.json`, `schema.gen.ts` | the test operation |

## 1. `alerts/message.rs`

Imports: `serde::Serialize`, `uuid::Uuid`, `llmobs_core::ids::ProjectId`, `llmobs_core::names::PRODUCT`, `llmobs_core::time_serde`, `crate::api::alerts::{AlertMetric, AlertWindow, Comparator}`, `crate::config::PublicUrl`, `crate::email::Email` (adjust to what the compiler reports; run `just fmt`).

```rust
//! Notification bodies and links (spec 07 § Channels, D-K-8).

/// Why a message is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageKind { Fired, Resolved, Reminder, Test }

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
```

### Links

```rust
/// `<public_url>/<org_slug>/<project_slug>/alerts/<rule_id>`.
pub(crate) fn rule_url(public_url: &PublicUrl, org_slug: &str, project_slug: &str, rule_id: Uuid) -> String;

/// `<public_url>/<org_slug>/<project_slug>/traces?from=<rfc3339>&to=<rfc3339>[&filter=<text>]`,
/// the query built with `url::form_urlencoded::Serializer` (so the filter is percent-encoded).
pub(crate) fn traces_url(public_url: &PublicUrl, org_slug: &str, project_slug: &str, from: i64, to: i64, filter: Option<&str>) -> String;
```

The times are RFC 3339 UTC with 9 fractional digits, from a private helper in `message.rs` (`time_serde::format_rfc3339` is `pub(crate)` in `llmobs-core`, so it cannot be used; `chrono` is a server dependency):

```rust
fn rfc3339(ns: i64) -> String {
    let nanos = u32::try_from(ns.rem_euclid(1_000_000_000)).unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(ns.div_euclid(1_000_000_000), nanos)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        .unwrap_or_default()
}
```

The web's traces page accepts these (`web/src/lib/search/common.ts` `RFC3339`), and its router decodes `+` in a query value as a space (form encoding); K12's tests do not depend on it.

### Value text

```rust
/// How a value reads in a message: `12.5%` for `error_rate`, `$3.75` (2 decimals) for `cost`,
/// `820 ms` (0 decimals) for `latency_p95`, 3 decimals for `score` (`0.700`). `None` → `"no data"`.
pub(crate) fn value_text(metric: AlertMetric, value: Option<f64>) -> String;
```

`error_rate`: `format!("{:.1}%", value * 100.0)`. The threshold uses the same function (`value_text(metric, Some(threshold))`).

### Titles

| kind | title |
|---|---|
| Fired | `Firing: <rule name>` |
| Resolved | `Resolved: <rule name>` |
| Reminder | `Still firing: <rule name>` |
| Test | `Test notification from <PRODUCT>` (`llmobs_core::names::PRODUCT`) |

The rule name is cut to 100 characters (`chars().take(100)`) in every title. Build titles with `fn title(kind: MessageKind, rule_name: &str) -> String`; Slack's `"text"` calls it with the escaped name (`&slack_escape(&notice.rule_name)`), the header with the raw name.

### Slack

```rust
pub(crate) fn slack_body(notice: &Notice) -> serde_json::Value;
```

```json
{
  "text": "<title> — <value text> (<symbol> <threshold text>) over <window> in <project name>",
  "blocks": [
    {"type": "header", "text": {"type": "plain_text", "text": "<title>"}},
    {"type": "section", "fields": [
      {"type": "mrkdwn", "text": "*Project*\n<project name>"},
      {"type": "mrkdwn", "text": "*Metric*\n<metric as_str>"},
      {"type": "mrkdwn", "text": "*Value*\n<value text>"},
      {"type": "mrkdwn", "text": "*Threshold*\n<symbol> <threshold text>"},
      {"type": "mrkdwn", "text": "*Window*\n<window as_str>"},
      {"type": "mrkdwn", "text": "*Samples*\n<count, or —>"}
    ]},
    {"type": "actions", "elements": [
      {"type": "button", "text": {"type": "plain_text", "text": "View rule"}, "url": "<rule_url>"},
      {"type": "button", "text": {"type": "plain_text", "text": "View traces"}, "url": "<traces_url>"}
    ]}
  ]
}
```

Build it with `serde_json::json!`. In the `mrkdwn` texts and in `"text"`, escape the project name, the rule name **and the comparator symbol** with `fn slack_escape(text: &str) -> String` (`&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, in that order): an unescaped leading `>` is a block quote in Slack and `<` starts a link. So the threshold field reads `*Threshold*\n&gt; 5.0%` for `gt`. `plain_text` needs no escaping. The `—` is U+2014.

`slack_body` returns a `serde_json::Value`; the sender serializes it with `serde_json::to_vec(&value).unwrap_or_default()` (a `Value` always serializes; add that comment).

### Webhook

```rust
/// The webhook body (spec 07 as amended by D-K-8).
#[derive(Debug, Serialize)]
pub(crate) struct WebhookPayload<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,          // "alert.fired" | "alert.resolved" | "alert.reminder" | "alert.test"
    pub(crate) event_id: Uuid,
    pub(crate) rule: WebhookRule<'a>,
    pub(crate) project: WebhookProject<'a>,
    pub(crate) value: Option<f64>,          // JSON null when absent
    pub(crate) count: Option<i64>,
    pub(crate) threshold: f64,
    pub(crate) comparator: Comparator,      // "gt", "gte", "lt", "lte"
    pub(crate) window: AlertWindow,         // "5m", …
    #[serde(with = "time_serde")] pub(crate) window_from: i64,
    #[serde(with = "time_serde")] pub(crate) window_to: i64,
    #[serde(with = "time_serde")] pub(crate) occurred_at: i64,
    pub(crate) url: &'a str,                // traces_url
    pub(crate) rule_url: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct WebhookRule<'a> { pub(crate) id: Uuid, pub(crate) name: &'a str, pub(crate) metric: AlertMetric }

#[derive(Debug, Serialize)]
pub(crate) struct WebhookProject<'a> { pub(crate) id: ProjectId, pub(crate) name: &'a str }

pub(crate) fn webhook_body(notice: &Notice) -> Vec<u8>;   // serde_json::to_vec(..).unwrap_or_default(), with a comment: the payload holds strings, numbers and UUIDs only
```

`value` and `count` are written as JSON `null` when absent (no `skip_serializing_if`), so receivers see every key.

### Email

In `email/mod.rs` add the variant:

```rust
    /// An alert notification (D-K-8).
    Alert {
        /// `Firing`, `Resolved`, `Still firing` or `Test`.
        state: &'static str,
        rule_name: String,
        project_name: String,
        value: String,
        threshold: String,
        window: &'static str,
        rule_url: String,
        traces_url: String,
    },
```

- `TEMPLATES`: add `("alert.txt", include_str!("templates/alert.txt"))` and `("alert.html", include_str!("templates/alert.html"))`.
- `render`: `Email::Alert { … } => ("alert.txt", "alert.html", context! { state => state, rule_name => rule_name, project_name => project_name, value => value, threshold => threshold, window => window, rule_url => rule_url, traces_url => traces_url })`.
- `subject`: `Email::Alert { state, rule_name, .. } => format!("[{PRODUCT}] {state}: {rule_name}")`.
- `template_name`: `"alert"`.

`alert.txt`:

```
{{ state }}: {{ rule_name }} in {{ project_name }}
Value: {{ value }} (threshold {{ threshold }}) over {{ window }}

View the rule:
{{ rule_url }}

View the traces:
{{ traces_url }}
```

`alert.html`: copy `test.html`'s layout; inside the cell put `<p style="margin:0 0 16px;font-weight:600;">{{ state }}: {{ rule_name }}</p>`, `<p style="margin:0 0 16px;">{{ project_name }} · {{ value }} (threshold {{ threshold }}) over {{ window }}</p>`, and two links `<a href="{{ rule_url }}">View the rule</a>` and `<a href="{{ traces_url }}">View the traces</a>` in one `<p>`. `.html` is auto-escaped.

Extend `templates_render` with `insta::assert_snapshot!("alert", rendered(&mailer, &Email::Alert { state: "Firing", rule_name: "High errors".to_owned(), project_name: "Support bot".to_owned(), value: "12.5%".to_owned(), threshold: "5.0%".to_owned(), window: "5m", rule_url: "https://example.com/acme/support-bot/alerts/r".to_owned(), traces_url: "https://example.com/acme/support-bot/traces?from=a&to=b".to_owned() }))`. Accept the new snapshot (`INSTA_UPDATE=always cargo test -p llmobs-server templates_render`) and check it reads right.

```rust
/// The email for `notice`.
pub(crate) fn email(notice: &Notice) -> Email;
```

`state` is `"Firing"`, `"Resolved"`, `"Still firing"` or `"Test"`.

## 2. Store additions (`meta/alerts.rs`)

```rust
/// A delivery whose attempt is due, with what the message needs.
#[derive(Clone, PartialEq)]
pub(crate) struct DueDelivery {
    pub(crate) id: Uuid,
    pub(crate) attempts: i64,
    pub(crate) channel_id: Uuid,
    pub(crate) channel_kind: ChannelKind,
    pub(crate) config_enc: Vec<u8>,
    pub(crate) event: EventRow,
    pub(crate) project_id: ProjectId,
    pub(crate) project_name: String,
    pub(crate) project_slug: String,
    pub(crate) org_slug: String,
}

/// `pending` deliveries with `next_attempt_at <= now`, oldest `next_attempt_at` first, then `id`, at most `limit`.
pub(crate) async fn due_deliveries(pool: &SqlitePool, now: i64, limit: i64) -> Result<Vec<DueDelivery>, MetaError>;

/// The result of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Attempt {
    Sent,
    /// Try again at `next_attempt_at` (already computed by the caller).
    Retry { error: String, next_attempt_at: i64 },
    Failed { error: String },
}

/// Records one attempt: `attempts += 1`, `updated_at = now`, and the status from `attempt`.
/// Only a row still `pending` is changed; returns whether it was.
pub(crate) async fn finish_delivery(conn: &mut SqliteConnection, id: Uuid, attempt: Attempt, now: i64) -> Result<bool, MetaError>;

/// Deletes events created before `events_before` (their deliveries cascade) and `sent`/`failed`
/// deliveries updated before `deliveries_before`. Returns `(events, deliveries)` deleted.
pub(crate) async fn prune(conn: &mut SqliteConnection, events_before: i64, deliveries_before: i64) -> Result<(u64, u64), MetaError>;
```

`DueDelivery` implements `Debug` by hand and prints only `id`, `attempts`, `channel_id` and `channel_kind` (`finish_non_exhaustive`): its event holds the rule name and filter, which never reach a log.

Decoding: a private `#[derive(sqlx::FromRow)] struct DueRecord` with one field per selected column (`String` for ids and texts, `Vec<u8>` for `config_enc`, `i64`/`Option<i64>`/`Option<f64>` as the column types). Parse `channel_kind` with `ChannelKind::parse`, `event_kind` with `AlertEventKind::parse`, `details` with `serde_json::from_str::<EventDetails>`, ids with `.parse()`. A failure → `MetaError::Corrupt { table: "notification_deliveries", column }` with `column` one of `"id"`, `"kind"`, `"details"`.

`due_deliveries` SQL (one static query):

```sql
SELECT d.id, d.attempts, d.channel_id, c.kind AS channel_kind, c.config_enc,
       e.id AS event_id, e.rule_id, e.kind AS event_kind, e.value, e.count, e.threshold,
       e.window_from, e.window_to, e.details, e.created_at AS event_created_at,
       p.id AS project_id, p.name AS project_name, p.slug AS project_slug, o.slug AS org_slug
FROM notification_deliveries d
JOIN notification_channels c ON c.id = d.channel_id
JOIN alert_events e ON e.id = d.event_id
JOIN alert_rules r ON r.id = e.rule_id
JOIN projects p ON p.id = r.project_id
JOIN orgs o ON o.id = p.org_id
WHERE d.status = 'pending' AND d.next_attempt_at <= ? AND p.state = 'active' AND o.state = 'active'
ORDER BY d.next_attempt_at, d.id
LIMIT ?
```

`finish_delivery` takes `attempt` by value and `match`es it by value (moving `error` out). One static `UPDATE notification_deliveries SET status = ?, attempts = attempts + 1, next_attempt_at = ?, last_error = ?, updated_at = ? WHERE id = ? AND status = 'pending'` with `('sent', NULL, NULL)`, `('pending', next, error)` or `('failed', NULL, error)`.

`prune`: `DELETE FROM alert_events WHERE created_at < ?`, then `DELETE FROM notification_deliveries WHERE status IN ('sent', 'failed') AND updated_at < ?`; `rows_affected()` of each.

## 3. `alerts/deliver.rs`

### Constants

```rust
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
```

### Backoff

```rust
/// Wait before attempt `attempts + 1`, after `attempts` failed: `min(30 s · 2^(attempts − 1), 1 h)`.
pub(crate) fn backoff_ns(attempts: i64) -> i64;
```

`attempts` is ≥ 1. 1 → 30 s, 2 → 60 s, 3 → 120 s, …, 7 → 1 920 s, 8 and more → 3 600 s. Exactly:

```rust
pub(crate) fn backoff_ns(attempts: i64) -> i64 {
    const BASE: i64 = 30_000_000_000;
    const CAP: i64 = 3_600_000_000_000;
    let shift = u32::try_from(attempts.saturating_sub(1).clamp(0, 20)).unwrap_or(20);
    BASE.saturating_mul(1_i64 << shift).min(CAP)
}
```

### One send

```rust
/// Why a send failed. The message never holds a URL, a header value, a secret or an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendError {
    pub(crate) retryable: bool,
    pub(crate) message: String,
}

/// Sends `notice` once to the channel `config`. `delivery_id` goes into `X-Llmobs-Delivery`.
pub(crate) async fn send_one(state: &AppState, config: &ChannelConfig, notice: &Notice, delivery_id: Uuid, now: i64) -> Result<(), SendError>;
```

- **Slack**: POST `slack_body(notice)` (`serde_json::to_vec`) to `config.url` with header `content-type: application/json`.
- **Webhook**: POST `webhook_body(notice)` to `config.url` with headers `content-type: application/json`, `x-llmobs-signature: sign_payload(&Secret::new(secret.clone()), now / 1_000_000_000, &body)`, `x-llmobs-delivery: <delivery_id>`, then each configured header (validated at write time; a header that no longer converts → permanent `"invalid header"`). `sign_payload` returning `None` → permanent `"could not sign the payload"`.
- Both: parse the URL with `reqwest::Url::parse` (failure → permanent `"invalid URL"`), then `state.eval_runner.egress().post(EgressRequest { url: &url, headers, body, timeout: SEND_TIMEOUT, max_response_bytes: MAX_RESPONSE_BYTES })`. Results:

| Result | retryable | message |
|---|---|---|
| status 200–299 | — | (success) |
| status 408, 429, 500–599 | true | `the endpoint answered <status>` |
| any other status | false | `the endpoint answered <status>` |
| `EgressError::Timeout` | true | `the request timed out` |
| `EgressError::Connect` | true | `could not connect` |
| `EgressError::Request` | true | `the request failed` |
| `EgressError::Denied { .. }` | false | `the address is not allowed (security.allow_private_network_egress)` |
| `EgressError::Scheme` | false | `unsupported URL scheme` |
| `EgressError::TooLarge { .. }` | false | `the response was too large` |

  (These six are all the `EgressError` variants.)
- **Email**: `state.mailer` is `None` → permanent `email is not configured`. Otherwise `let email = message::email(notice);` and for each recipient in order `mailer.send_now(recipient, &email).await`; stop at the first error. Wrap the whole loop in `tokio::time::timeout(SEND_TIMEOUT, …)`; an elapsed timeout → retryable `the request timed out`. `MailError::Connect`, `Tls` and `Auth` → retryable (an admin may fix SMTP credentials within the retry span, D-K-7); `Rejected`, `InvalidAddress`, `Template` → permanent. The message is the error's `Display` text (fixed strings without the address, `email/mod.rs`). D-K-7: a retry sends to every recipient again.

### A pass

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeliverReport { pub(crate) sent: usize, pub(crate) retried: usize, pub(crate) failed: usize }

/// Sends every due delivery once, at most [`CONCURRENCY`] at a time.
pub(crate) async fn deliver_once(state: &AppState, now: i64) -> Result<DeliverReport, MetaError>;
```

`async fn deliver_one(state: &AppState, due: &DueDelivery, now: i64) -> Option<Outcome>` with a private `enum Outcome { Sent, Retried, Failed }`; it returns `None` when the final write fails. Run it as `futures::stream::iter(&dues).map(|due| Box::pin(deliver_one(state, due, now))).buffer_unordered(CONCURRENCY)` (borrowed, so no `needless_pass_by_value`; boxed because reqwest futures are large). For each `DueDelivery`:
1. `open_config(&state.secrets, &due.channel_id, &due.config_enc)`; error → `Attempt::Failed { error: "the channel config could not be read".to_owned() }`.
2. Build the `Notice`: `kind` from `due.event.kind`, an `AlertEventKind` (`match due.event.kind { AlertEventKind::Fired => MessageKind::Fired, AlertEventKind::Resolved => MessageKind::Resolved, AlertEventKind::Reminder => MessageKind::Reminder }`); rule name, metric, comparator, window, filter from `due.event.details`; value, count, threshold, window bounds from the event; `occurred_at = due.event.created_at`; links from `rule_url(&state.config.public_url, &due.org_slug, &due.project_slug, due.event.rule_id)` and `traces_url(…, due.event.window_from, due.event.window_to, due.event.details.filter.as_deref())`.
3. `send_one(state, &config, &notice, due.id, now)`:
   - `Ok` → `Attempt::Sent`;
   - `Err(e)` with `e.retryable && due.attempts + 1 < MAX_ATTEMPTS` → `Attempt::Retry { error: e.message, next_attempt_at: now.saturating_add(backoff_ns(due.attempts + 1)) }`;
   - otherwise → `Attempt::Failed { error: e.message }`.
   Compute the `Outcome` and keep a copy of the error message for the log before moving `attempt` into the write closure.
4. `state.meta.write(… finish_delivery(conn, due.id, attempt, now) …)`. The report counts the outcome (`sent`, `retried`, `failed`) whether or not `finish_delivery` returned `true`. A write error is logged, not counted, and the pass continues (the row stays `pending` and is retried next pass).
5. Log: `tracing::info!(delivery_id = %due.id, channel_id = %due.channel_id, kind = due.channel_kind.as_str(), attempt = due.attempts + 1, outcome, "alert delivery")` where `outcome` is `"sent"`, `"retry"` or `"failed"`; for a failure add `error = %message` (the fixed message, which holds no secret).

### Pruning

```rust
/// Deletes events older than 90 days and finished deliveries older than 30 days (D-K-10).
pub(crate) async fn prune_once(state: &AppState, now: i64) -> Result<(u64, u64), MetaError>;
```

`meta.write(prune(conn, now − EVENT_RETENTION_NS, now − DELIVERY_RETENTION_NS))`; log `tracing::info!(events, deliveries, "alert history pruned")` when either is non-zero.

## 4. "Send test"

`api/alerts.rs`:

```rust
/// The result of `POST /channels/{id}/test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelTestResult {
    /// The channel accepted the message.
    pub ok: bool,
    /// Why it did not. Never contains a URL, a secret or an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

`routes/channels.rs`: a fifth `RouteDef` — `POST /api/v1/projects/{project}/channels/{id}/test`, `ProjectPerm::ManageAlerts`, not session-only, documented, handler:

```rust
async fn test_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(ChannelPath { id }): error::Path<ChannelPath>,
) -> Result<error::Json<ChannelTestResult>, ApiError>
```

`let now = state.clock.now_ns();`

1. `session_user`; `alerts::channel_by_id` (404 when absent); `open_config` (error → `200 {ok: false, error: "the channel config could not be read"}`).
2. Build a sample `Notice`: `kind Test`, `event_id = Uuid::now_v7()`, `rule_id = Uuid::nil()`, `rule_name "Test alert"`, `metric ErrorRate`, `comparator Gt`, `threshold 0.05`, `window M5`, `filter None`, `value Some(0.125)`, `count Some(48)`, `window_from = now − 5 min`, `window_to = now`, `occurred_at = now`, the project's id and name (`access.project.name`), `rule_url = state.config.public_url.join(&format!("/{org_slug}/{project_slug}/alerts"))` (the list page; `project_slug = access.project.slug`; the org slug from `crate::meta::orgs::org_by_id(state.meta.reader(), access.project.org_id)`, where `None` → `ApiError::NotFound`), `traces_url` over the sample window without a filter.
3. `Box::pin(send_one(&state, &config, &notice, Uuid::now_v7(), now)).await` → `200 {ok: true}` or `200 {ok: false, error}`. Nothing is stored.
4. Audit log `"alert channel tested"` with `channel_id`, `project_id`, `user_id`, `ok`.

OpenAPI: `test_channel` — POST `{PROJECT}/channels/{id}/test`, tag `"alerts"`, `.parameters(Some(vec![project_param(), path_uuid("id", "Channel id.")]))` (copy K5's `delete_channel` builder), summary `Send a test notification`, description `"Sends one sample message now and reports the result. Nothing is stored. Needs role member."`, `200` `json_of::<ChannelTestResult>("Whether the channel accepted the message.")`, errors `&[400, 401, 403, 404, 429]`; `set.insert::<ChannelTestResult>()`. `tests/openapi.rs`: the operation in `OPERATIONS`, and `test_channel` in the `["400", "401", "403", "404", "429"]` branch. `just openapi`.

## 5. Test receiver (`test_support/hook_receiver.rs`)

Register `pub(crate) mod hook_receiver;` in `test_support/mod.rs`.

```rust
//! An HTTP endpoint for delivery tests: answers scripted statuses and records every request.

/// One recorded request.
#[derive(Debug, Clone)]
pub(crate) struct Received {
    pub(crate) path: String,
    pub(crate) headers: axum::http::HeaderMap,
    pub(crate) body: Vec<u8>,
}

/// Serves any path on 127.0.0.1:0. Answers the scripted statuses in order; the last one repeats
/// (200 when none was scripted). Aborts its task on drop.
pub(crate) struct HookReceiver { /* addr, state: Arc<Mutex<…>>, task: JoinHandle<()> */ }

impl HookReceiver {
    pub(crate) async fn start(statuses: Vec<u16>) -> Self;
    /// `http://127.0.0.1:<port><path>`.
    pub(crate) fn url(&self, path: &str) -> String;
    pub(crate) fn received(&self) -> Vec<Received>;
}
```

Implement with an axum `Router::new().fallback(handler)` served by `axum::serve(tokio::net::TcpListener::bind("127.0.0.1:0"), …)` on a spawned task; the handler takes `axum::extract::Request`, reads the body with `axum::body::to_bytes(body, 1 << 20)`, records it, and answers `StatusCode::from_u16(status)` with an empty body. `impl Drop for HookReceiver { fn drop(&mut self) { self.task.abort(); } }`. (I's `test_support::mock_llm` is a similar helper; read it if it exists and follow its style.)

Tests that send to it need `TestApp::with_config(|config| config.security.allow_private_network_egress = true)` (127.0.0.1 is private). For email tests use `TestApp::with_mailer_config(|config| config.security.allow_private_network_egress = true)` or `with_mailer()`.

## 6. Tests

Shared test helpers (in `alerts::deliver::tests`, `pub(super)` where `message::tests` needs them):

```rust
/// A sample notice: error_rate, gt 0.05, 5m, value 0.125, count 48, window [now − 5 min, now).
fn sample_notice(kind: MessageKind, now: i64) -> Notice;

/// Seeds a project (`app.seed()`), inserts a channel with `config` sealed by `seal_config`,
/// a rule (error_rate, 5m, gt 0.05) using it, and records a `Fired` evaluation at `now`
/// through `record_evaluation` (`Step { state: Firing, clear_streak: 0, event: Some(Fired),
/// fired: true, notified: true }`, count 48, value 0.125, threshold 0.05,
/// window [now − 300 s, now)), so one delivery is `pending` with `next_attempt_at = now`.
/// Returns (delivery id, channel id, event id).
async fn seed_delivery(app: &TestApp, config: ChannelConfig, now: i64) -> (Uuid, Uuid, Uuid);
```

`PublicUrl` has no public constructor: build one with `serde_json::from_str::<PublicUrl>("\"https://obs.example.com\"").expect("url")`.

`alerts::message::tests`:
1. `links`: `rule_url` with public URL `https://obs.example.com` → `https://obs.example.com/acme/bot/alerts/<id>`; `traces_url` with filter `environment = 'prod' & x`, `from = 0`, `to = 300_000_000_000` → the query contains `filter=environment+%3D+%27prod%27+%26+x`, `from=1970-01-01T00%3A00%3A00.000000000Z` and `to=1970-01-01T00%3A05%3A00.000000000Z` (form encoding turns `:` into `%3A`).
2. `value_texts`: `error_rate 0.125` → `12.5%`; `cost 3.754` → `$3.75`; `latency_p95 820.4` → `820 ms`; `score 0.7` → `0.700`; `None` → `no data`.
3. `slack_body_shape`: the header text is `Firing: <name>`; there are 6 fields; the two buttons carry the URLs; a rule named `<b>&` appears escaped as `&lt;b&gt;&amp;` in `"text"` and unescaped in the header; the threshold field is `*Threshold*\n&gt; 5.0%` for `gt` and contains `&lt;=` for `lte`; `"text"` contains `(&gt; 5.0%)`.
4. `webhook_body_shape`: `type "alert.resolved"`, `event_id`, `rule.metric "error_rate"`, `comparator "gt"`, `window "5m"`, `value null` and `count null` present when absent, times as RFC 3339 strings.

`alerts::deliver::tests`:
5. `backoff_doubles_and_caps`: 1 → 30 s, 2 → 60 s, 7 → 1 920 s, 8 → 3 600 s, 100 → 3 600 s.
6. `webhook_is_signed_and_identified`: receiver `[200]`; webhook channel with secret `0123456789abcdef` and header `X-Team: core`; `send_one` → `Ok`. The request has `content-type: application/json`, `x-team: core`, `x-llmobs-delivery: <id>`, and an `x-llmobs-signature` equal to `sign_payload(&Secret::new(secret), now / 1_000_000_000, &body)`.
7. `slack_posts_block_kit`: receiver `[200]`; a Slack channel whose URL is the receiver's (http is fine for the test: only validation requires https) → `Ok`, the body parses as JSON with `blocks`.
8. `statuses_are_classified`: receiver `[503]` → `Err { retryable: true, message: "the endpoint answered 503" }`; `[404]` → retryable false; `[429]` → retryable true.
9. `private_addresses_are_denied_by_default`: `TestApp::new()` (policy default) → sending to the receiver gives `Err { retryable: false, message: "the address is not allowed (security.allow_private_network_egress)" }`.
10. `deliver_once_retries_then_fails`: `seed_delivery` with a webhook channel pointing at the receiver `[500]`. Call `deliver_once` at `now`: `retried 1`, the row is `pending` with `attempts 1`, `next_attempt_at = now + 30 s`, `last_error "the endpoint answered 500"`. Calling at `now + 29 s` sends nothing (`DeliverReport::default()`). Loop 7 more times at the row's `next_attempt_at`: after the 8th attempt the row is `failed`, `next_attempt_at NULL`; the receiver got 8 requests.
11. `deliver_once_sends`: receiver `[200]` → `sent 1`, row `sent`, `attempts 1`; a second pass sends nothing.
12. `email_delivery_sends_to_each_recipient`: `with_mailer`; email channel `["a@x.io","b@x.io"]` → `deliver_once` → `app.sent()` has 2 messages, subject `[<PRODUCT>] Firing: <rule name>`; that is all for this test (`seed` runs once per app).
13. `email_without_smtp_fails_at_once`: `TestApp::new()`, `seed_delivery` with an email config → `failed`, `last_error "email is not configured"`.
14. `prune_keeps_recent_history`: an old event (`created_at = now − 91 d`) with one `sent` delivery; a recent event (`now − 1 d`) with a `sent` delivery updated `now − 31 d`, a `failed` delivery updated `now − 1 d`, and a `pending` delivery updated `now − 40 d` (insert them by SQL). `prune_once` returns `(1, 1)` (cascaded deletes are not counted by SQLite); afterwards the old event and its delivery are gone, and the recent event, its `failed` delivery and the `pending` one remain.
17. `finish_delivery_is_fenced`: after `Attempt::Sent`, a second `finish_delivery(…, Attempt::Failed { … })` returns `false` and `attempts` stays 1.
18. `due_deliveries_order_and_filter`: three pending deliveries with `next_attempt_at` 30, 10, 20 and one `sent` → `due_deliveries(now = 25, limit 10)` returns the ones at 10 and 20, in that order; `limit 1` returns one; after setting the project's `state = 'deleting'`, none.
15. `delivery_logs_hold_no_secrets` (a default current-thread `#[tokio::test]`): `seed_delivery` with a webhook (path `/hook`, secret `0123456789abcdef`, header `X-Team: core`) to a receiver `[500]`; wrap `deliver_once(&app.state, now)` in `capture_logs` and assert the captured text contains `alert delivery` and none of `0123456789abcdef`, `"core"`, `/hook`; then the same around a second pass at `now + 31 s` (the first left the row due at `now + 30 s`).
12b. `email_retry_after_a_failure`: `TestApp::with_mailer()`, then `app.state.mailer.as_ref().expect("stub mailer").fail_next(1)`, then `seed_delivery` with an email config → `deliver_once` reports `retried 1`, the row is `pending` with `attempts 1`, and `app.sent()` is empty.
19. `permanent_failure_and_unreadable_config`: a receiver `[404]` → `failed`, `attempts 1`, `next_attempt_at NULL`, `last_error "the endpoint answered 404"`; a channel whose `config_enc` is sealed for another id (update the row by SQL) → `failed` with `"the channel config could not be read"`.

`routes::channels::tests`:
16. `send_test_reports_the_result`: `TestApp::with_config(|config| config.security.allow_private_network_egress = true)`; two receivers, `[200]` and `[500]`; create two webhook channels over `POST /channels` (`http://127.0.0.1:<port>/hook`, secret `0123456789abcdef`), one per receiver. Testing the first → `200 {"ok": true}` and its receiver got a body with `"type":"alert.test"`; the second → `{"ok": false, "error": "the endpoint answered 500"}`; a viewer → `403`; an unknown id → `404`.

## Commands

```bash
just fmt
```

```bash
cargo test -p llmobs-server alerts::
```

```bash
cargo test -p llmobs-server channels
```

```bash
INSTA_UPDATE=always cargo test -p llmobs-server templates_render
```

```bash
just openapi
```

```bash
just check
```

## Done when

- Deliveries are sent with the classification and backoff above, a channel test answers synchronously, and pruning keeps 90 and 30 days.
- The 20 tests and the updated `templates_render` pass; `just check` is green. If clippy's `large_futures` flags another await on a reqwest or DataFusion future, wrap it in `Box::pin(…)` as above.
