# K6 — Rule routes, event history, OpenAPI and fixtures

**Commit subject:** `server: add alert rule and event routes`

Adds the alert rule CRUD and the per-rule event history (spec 09 § Evaluation & alerts):

```
GET    /api/v1/projects/{project}/alert-rules             View          list (all rules, by name)
POST   /api/v1/projects/{project}/alert-rules             ManageAlerts  create
GET    /api/v1/projects/{project}/alert-rules/{id}        View          one rule
PATCH  /api/v1/projects/{project}/alert-rules/{id}        ManageAlerts  update
DELETE /api/v1/projects/{project}/alert-rules/{id}        ManageAlerts  delete
GET    /api/v1/projects/{project}/alert-rules/{id}/events View          events, newest first, paged, with their deliveries
```

It also adds the store reads for events and deliveries (K7 and K8 write them).

## Read first

- README § Design, D-K-2, D-K-4, D-K-5, D-K-9, § Conventions.
- K3 (`api::alerts`, `alerts::condition`), K4 (`meta::alerts`), K5 (`routes/channels.rs`, the OpenAPI and fixture steps; this sub-phase repeats them for rules).
- `crates/server/src/routes/api_keys.rs`: `page_keys` and its cursor (base64url of JSON `{created_at, id}`).
- `crates/server/src/routes/read.rs`: `open_query`.
- `crates/storage` K1: `check_alert_filter`.
- `crates/server/src/http/error.rs`: how `RunError::Invalid` becomes `400 invalid_filter` with field `filter`.

## Files

| File | Change |
|---|---|
| `crates/server/src/api/alerts.rs` | rule and event types |
| `crates/server/src/meta/alerts.rs` | `EventDetails`, `EventRow`, `DeliveryRow`, `list_events`, `deliveries_for_events` |
| `crates/server/src/routes/alert_rules.rs` | new |
| `crates/server/src/alerts/quota.rs`, `alerts/mod.rs` | the plan limit check (D-K-14) |
| `crates/server/src/api/common.rs`, `crates/server/src/http/error.rs` | `ProblemCode::PlanLimit`, `ApiError::PlanLimit` |
| `crates/server/src/routes/mod.rs` | `pub(crate) mod alert_rules;` (alphabetical: before `api_keys`) |
| `crates/server/src/http/access.rs` | `routes.extend(crate::routes::alert_rules::routes());` |
| `crates/server/src/api/openapi.rs`, `crates/server/tests/openapi.rs` | as K5 |
| `crates/server/openapi.json`, `web/src/api/schema.gen.ts` | regenerated |
| `fixtures/api/static/alert-rules.json`, `alert-rule.json`, `alert-events.json` | new |
| `crates/server/src/api_examples.rs`, `fixtures/api/manifest.json`, `crates/server/tests/api_examples.rs` | three examples |

## 1. Types in `api/alerts.rs`

```rust
/// One alert rule (D-K-2, D-K-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlertRule {
    /// Rule id.
    pub id: Uuid,
    /// Display name, unique in the project.
    pub name: String,
    /// Disabled rules are not evaluated.
    pub enabled: bool,
    /// What is measured and when it breaches, with defaults filled in.
    pub condition: AlertCondition,
    /// Channels notified on fired, resolved and reminder events. Ordered by id.
    pub channel_ids: Vec<Uuid>,
    /// Minimum seconds between two reminders while firing.
    pub cooldown_s: i64,
    /// Send a reminder every `cooldown_s` while firing.
    pub notify_while_firing: bool,
    /// Current state.
    pub state: AlertState,
    /// Value of the last successful evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_value: Option<f64>,
    /// Sample count of the last successful evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_count: Option<i64>,
    /// Last evaluation, successful or not.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::api::keys::optional_time")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_evaluated_at: Option<i64>,
    /// Last `fired` event.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::api::keys::optional_time")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_fired_at: Option<i64>,
    /// Why the last evaluation failed. Absent after a successful one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Creation time.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last change.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// Body of `POST /alert-rules`.
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAlertRuleRequest {
    /// 1–100 characters after trimming.
    pub name: String,
    /// The condition. Defaults are filled in.
    pub condition: AlertCondition,
    /// At most 10 channels of this project. Default none.
    #[serde(default)]
    pub channel_ids: Vec<Uuid>,
    /// Default true.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 300–604800. Default 3600.
    #[serde(default)]
    pub cooldown_s: Option<i64>,
    /// Default false.
    #[serde(default)]
    pub notify_while_firing: Option<bool>,
}

/// Body of `PATCH /alert-rules/{id}`. An absent field keeps its value. A new
/// `condition`, or a change of `enabled`, resets the state to `ok` (D-K-4).
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAlertRuleRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub condition: Option<AlertCondition>,
    #[serde(default)]
    pub channel_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub cooldown_s: Option<i64>,
    #[serde(default)]
    pub notify_while_firing: Option<bool>,
}

/// What an event records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertEventKind {
    /// `ok` or `no_data` → `firing`.
    Fired,
    /// `firing` → `ok` or `no_data`.
    Resolved,
    /// Still firing, sent every `cooldown_s` when `notify_while_firing` is on.
    Reminder,
}

impl AlertEventKind {
    /// `fired`, `resolved`, `reminder`.
    #[must_use]
    pub fn as_str(self) -> &'static str { /* match */ }
    /// Parses the stored spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> { /* match */ }
}

/// One delivery of an event to a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlertDelivery {
    /// Delivery id; the webhook's `X-Llmobs-Delivery`.
    pub id: Uuid,
    /// The channel.
    pub channel_id: Uuid,
    /// The channel's current name.
    pub channel_name: String,
    /// The channel's kind.
    pub channel_kind: ChannelKind,
    /// Where the delivery stands.
    pub status: DeliveryStatus,
    /// Attempts made so far.
    pub attempts: i64,
    /// The last attempt's error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Last change.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// One fired, resolved or reminder event, with the rule as it was then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlertEvent {
    /// Event id; the webhook payload's `event_id`.
    pub id: Uuid,
    /// The rule.
    pub rule_id: Uuid,
    /// What happened.
    pub kind: AlertEventKind,
    /// The measured value. Absent when there was none (e.g. resolved by too few samples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// The sample count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// The threshold then.
    pub threshold: f64,
    /// The comparator then.
    pub comparator: Comparator,
    /// The metric then.
    pub metric: AlertMetric,
    /// The window then.
    pub window: AlertWindow,
    /// Window start.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub window_from: i64,
    /// Window end (exclusive).
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub window_to: i64,
    /// When the event was recorded.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// One per channel the rule had then.
    pub deliveries: Vec<AlertDelivery>,
}
```

Add `///` doc lines to every field of `UpdateAlertRuleRequest` (copy the matching ones of `CreateAlertRuleRequest`, prefixed with "New ").

`optional_time`: reuse `crate::api::keys::optional_time` (already `pub(crate)`; `api/connections.rs` uses it): write `#[serde(default, skip_serializing_if = "Option::is_none", with = "crate::api::keys::optional_time")]` on `last_evaluated_at` and `last_fired_at`. Do not move it.

`AlertEvent` exposes the snapshot's `metric`, `comparator` and `window` only; the rule name and filter stay in the stored `details` (K8's messages use them).

PATCH semantics: every field is a plain `Option` (`null` is the same as absent); `{}` changes nothing but `updated_at`; `"channel_ids": []` removes every channel.

## 2. Store reads in `meta/alerts.rs`

```rust
/// The rule as it was when the event was recorded. Stored as `alert_events.details`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EventDetails {
    pub(crate) rule_name: String,
    pub(crate) metric: AlertMetric,
    pub(crate) comparator: Comparator,
    pub(crate) window: AlertWindow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<String>,
}

/// One stored event.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EventRow {
    pub(crate) id: Uuid,
    pub(crate) rule_id: Uuid,
    pub(crate) kind: AlertEventKind,
    pub(crate) value: Option<f64>,
    pub(crate) count: Option<i64>,
    pub(crate) threshold: f64,
    pub(crate) window_from: i64,
    pub(crate) window_to: i64,
    pub(crate) details: EventDetails,
    pub(crate) created_at: i64,
}

/// One stored delivery with its channel's name and kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryRow {
    pub(crate) id: Uuid,
    pub(crate) event_id: Uuid,
    pub(crate) channel_id: Uuid,
    pub(crate) channel_name: String,
    pub(crate) channel_kind: ChannelKind,
    pub(crate) status: DeliveryStatus,
    pub(crate) attempts: i64,
    pub(crate) next_attempt_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// Events of `rule_id`, newest first (`created_at DESC, id DESC`), strictly before
/// `before` when given, at most `limit` rows.
pub(crate) async fn list_events(pool: &SqlitePool, rule_id: Uuid, before: Option<(i64, Uuid)>, limit: i64) -> Result<Vec<EventRow>, MetaError>;

/// Deliveries of the given events, grouped in the order of `event_ids`, then by `created_at`, then `id`.
pub(crate) async fn deliveries_for_events(pool: &SqlitePool, event_ids: &[Uuid]) -> Result<Vec<DeliveryRow>, MetaError>;
```

- `list_events` SQL, one static query: `SELECT … FROM alert_events WHERE rule_id = ?1 AND (?2 IS NULL OR created_at < ?2 OR (created_at = ?2 AND id < ?3)) ORDER BY created_at DESC, id DESC LIMIT ?4`, binding `before.map(|b| b.0)` and `before.map(|b| b.1.to_string())` as `Option`s. UUIDv7 text sorts by time, so `id < ?` on `TEXT` is the right tie-break.
- `deliveries_for_events`: empty input → `Ok(vec![])`. Run one static query per event id (a page holds at most 1 000 events; sqlx 0.9 accepts only static SQL strings, so do not build an `IN (…)` list): `SELECT d.id, d.event_id, d.channel_id, c.name AS channel_name, c.kind AS channel_kind, d.status, d.attempts, d.next_attempt_at, d.last_error, d.created_at, d.updated_at FROM notification_deliveries d JOIN notification_channels c ON c.id = d.channel_id WHERE d.event_id = ? ORDER BY d.created_at, d.id`, and concatenate the results in the order of `event_ids`.
- Decoding failures (`kind`, `status`, `details` JSON, ids) → `MetaError::Corrupt` with the table and column.
- Tests in the existing `mod tests`: insert events and deliveries by SQL. `events_page_newest_first` (5 events, `limit 2` → the 2 newest; `before` = the second's `(created_at, id)` → the next 2); `deliveries_join_channel_names` (two events with two deliveries each: the result follows the order of `event_ids`, then `created_at`, then `id`, and carries the channel's name and kind; an empty slice → empty vector).

## 3. `routes/alert_rules.rs`

### Route table

Six `RouteDef`s, all `Access::Project { …, session_only: false }`, `documented: true`: the table at the top of this file gives method, path and permission. Handlers: `list_rules`, `create_rule`, `get_rule`, `update_rule`, `delete_rule`, `list_events`. Path params: `struct RulePath { id: Uuid }` (`Deserialize`).

### Validation helpers (private)

```rust
const MIN_COOLDOWN_S: i64 = 300;
const MAX_COOLDOWN_S: i64 = 604_800;
const DEFAULT_COOLDOWN_S: i64 = 3_600;

fn checked_name(name: &str) -> Result<String, ApiError>;               // trim; "is required"; "at most 100 characters" (field "name")
fn checked_cooldown(value: i64) -> Result<i64, ApiError>;              // outside 300..=604800 → field "cooldown_s", "must be between 300 and 604800"
async fn checked_channels(state: &AppState, project: ProjectId, ids: &[Uuid]) -> Result<Vec<Uuid>, ApiError>;
async fn checked_condition(state: &AppState, project: ProjectId, condition: AlertCondition) -> Result<AlertCondition, ApiError>;
```

- `checked_channels`: more than `alerts::MAX_RULE_CHANNELS` (the K4 store constant) → field `channel_ids`, `"at most 10 channels"`; a duplicate id → `"duplicate channel"`; an id not in `list_channels(reader, project)` → `"unknown channel"`. Returns the ids sorted.
- `checked_condition`: `normalize_condition(condition)` (errors → `ApiError::BadRequest { detail: None, errors }`); then, when `filter` is `Some(text)`: `let ctx = open_query(state, project, None).await?; llmobs_storage::check_alert_filter(&ctx, &text)?;` (the `RunError` converts: an invalid filter is `400 invalid_filter` with field `filter`). Return the normalized condition.

### Handlers

Same extractors and write pattern as K5's handlers and `routes/api_keys.rs` (`State`, `Extension<Principal>`, `Extension<ProjectAccess>`, `error::Path`, `error::Json`, `state.meta.write(move |conn| Box::pin(async move { … })).await?`, `state.clock.now_ns()`, `access.project.id`). Copy the private `field_error` helper of `routes/api_keys.rs` into this file. Call the store through its module path (`alerts::list_rules`, …) because the handlers share its names; import `crate::meta::alerts::{self, NewRule, RulePatch, RuleRow}`. For the cursor: `use base64::Engine; use base64::engine::general_purpose::URL_SAFE_NO_PAD;`.

**`list_rules`**: `list_rules(reader, project)` → `Page { data: rows.map(rule_json), next_cursor: None }`. No query parameters.

**`create_rule`**, in order: `session_user`; `checked_name`; `checked_condition`; `checked_channels`; `checked_cooldown(body.cooldown_s.unwrap_or(DEFAULT_COOLDOWN_S))`; `insert_rule(conn, NewRule { project_id, name, condition, channel_ids, enabled: body.enabled.unwrap_or(true), cooldown_s, notify_while_firing: body.notify_while_firing.unwrap_or(false), created_by: Some(user.id) }, now)`; audit log `tracing::info!(target: "audit", rule_id = %row.id, project_id = %project_id, user_id = %user.id, metric = ?row.condition.metric, "alert rule created")`; `201` with `rule_json(&row)`.

**`get_rule`**: `rule_by_id`; `None` → 404.

**`update_rule`**: `session_user`; check each present field with the helpers above; `update_rule(conn, project, id, RulePatch { … }, now)`; `None` → 404; audit `"alert rule updated"`; `200` with the row.

**`delete_rule`**: `session_user`; `delete_rule`; `false` → 404; audit `"alert rule deleted"`; `204`.

**`list_events`**: `error::Query(params): error::Query<PageParams>`. `rule_by_id` first (404 when the rule is not in this project). Decode `params.cursor` as `api_keys.rs` does (base64url `URL_SAFE_NO_PAD` of JSON `{"created_at": i64, "id": Uuid}`, `deny_unknown_fields`; a bad cursor → `field_error("cursor", "invalid cursor")`). `Limit::get()` returns `u32`: fetch `alerts::list_events(reader, id, before, i64::from(params.limit.get()) + 1)` and compare with `let limit = usize::try_from(params.limit.get()).unwrap_or(usize::MAX);`. When more than `limit` rows came back, drop the extra one and set `next_cursor` to the encoding of the last kept row's `(created_at, id)`. Then `deliveries_for_events` for the kept ids, and assemble `AlertEvent`s (deliveries grouped by `event_id`, in the store's order). `metric`, `comparator`, `window` come from `details`.

Write the cursor encode/decode as two private functions in this file, `encode_cursor(created_at: i64, id: Uuid) -> Result<String, ApiError>` (serialization errors → `ApiError::Internal(Box::new(error))`, as `api_keys.rs`) and `decode_cursor(text: &str) -> Result<(i64, Uuid), ApiError>`, mirroring `api_keys.rs` (copy its struct with `#[serde(deny_unknown_fields)]`).

`rule_json(row: &RuleRow) -> AlertRule` copies the fields (`state`, `last_*` included; `last_notified_at` and `clear_streak` are not exposed).

## 3b. Plan limit (D-K-14, D178)

On the cloud Free plan an org may have 1 enabled alert rule; paid plans and self-hosted instances have no limit (D178). Track P owns entitlements (D179) and is not built yet. K adds the one check P will feed, so P changes a single function body and nothing else.

**`crate::alerts::quota`** (new file `alerts/quota.rs`; `pub(crate) mod quota;` in `alerts/mod.rs`):

```rust
//! The per-org alert rule limit (D178, D-K-14).

use llmobs_core::ids::OrgId;

use crate::http::AppState;
use crate::http::error::ApiError;

/// The most enabled alert rules `org_id` may have; `None` means no limit.
///
/// Track P (D179) replaces this body with the value of the org's resolved `Entitlements`
/// (its maximum number of alert rules). Until then every org is unlimited, which is
/// the self-hosted behaviour (D172).
pub(crate) fn rule_limit(_state: &AppState, _org_id: OrgId) -> Option<i64> {
    None
}

/// `Err(ApiError::PlanLimit)` when one more enabled rule would exceed `limit`.
pub(crate) fn check_rule_quota(enabled_rules: i64, limit: Option<i64>) -> Result<(), ApiError> {
    match limit {
        Some(limit) if enabled_rules >= limit => Err(ApiError::PlanLimit {
            detail: format!(
                "Your plan allows {limit} enabled alert {}.",
                if limit == 1 { "rule" } else { "rules" }
            ),
        }),
        _ => Ok(()),
    }
}
```

**Error type.** In `api/common.rs`, add to `ProblemCode` after `EmailNotVerified`:

```rust
    /// The org's plan does not allow this (D178). The detail names the limit.
    PlanLimit,
```

with status `403` (add it to the `Self::Forbidden | Self::SsoRequired | Self::EmailNotVerified` arm of `status`, and give it the same reason phrase as `Forbidden` in `reason_phrase` or whichever match the compiler reports). In `http/error.rs`, add to `ApiError` after `Forbidden`:

```rust
    /// The org's plan limit is reached (D178).
    #[error("plan limit")]
    PlanLimit {
        /// Names the limit; never holds payload.
        detail: String,
    },
```

and in `problem()`: `Self::PlanLimit { detail } => Problem::new(ProblemCode::PlanLimit, Some(detail.clone())),`. Fix any other exhaustive `match` on `ApiError` or `ProblemCode` the compiler reports the same way. The OpenAPI `ProblemCode` enum gains `plan_limit` (regenerated by `just openapi`); spec 09's code list already names it (planning commit).

**Where it runs** (only when the result is an enabled rule). A private helper in `routes/alert_rules.rs`, so tests can pass a limit:

```rust
/// Refuses one more enabled rule in `org` when `limit` is reached (D-K-14).
async fn enforce_quota(state: &AppState, org: OrgId, limit: Option<i64>) -> Result<(), ApiError> {
    if limit.is_none() {
        return Ok(());
    }
    let enabled = alerts::count_enabled_org_rules(state.meta.reader(), org).await?;
    quota::check_rule_quota(enabled, limit)
}
```

- `create_rule`, after `checked_cooldown` and before the insert, when `body.enabled.unwrap_or(true)`: `enforce_quota(&state, org, quota::rule_limit(&state, org)).await?;` with `org = access.project.org_id`.
- `update_rule`: after validating the present fields, load the rule (`let Some(current) = alerts::rule_by_id(state.meta.reader(), project, id).await? else { return Err(ApiError::NotFound); };`); when `body.enabled == Some(true) && !current.enabled`, call `enforce_quota` the same way; then write.

Two concurrent requests can both pass and leave the org one rule over its limit; that is accepted (the next re-enable or create is refused).

**Tests** (`alerts::quota::tests` and `http::error` tests):
- `no_limit_allows_everything`: `check_rule_quota(5, None)` is `Ok`.
- `limit_one`: `check_rule_quota(0, Some(1))` is `Ok`; `check_rule_quota(1, Some(1))` is `Err(ApiError::PlanLimit { detail })` with `detail == "Your plan allows 1 enabled alert rule."`; `check_rule_quota(3, Some(3))` names `3 enabled alert rules`.
- `plan_limit_problem`: `ApiError::PlanLimit { detail: "x".into() }.problem()` has `status 403`, `code PlanLimit` (serialized `"plan_limit"`) and `detail "x"`.
- Route tests 12 and 13 below: with `rule_limit` returning `None`, more enabled rules are created (self-hosted has no limit); `enforce_quota` with `Some(1)` refuses a second enabled rule.

## 4. OpenAPI

`collect_schemas`: `AlertMetric`, `AlertWindow`, `Comparator`, `LatencyTarget`, `ScoreAggregation`, `AlertState`, `AlertCondition`, `AlertRule`, `CreateAlertRuleRequest`, `UpdateAlertRuleRequest`, `AlertEventKind`, `AlertDelivery`, `AlertEvent`, and `insert_page::<AlertRule>("AlertRulePage")`, `insert_page::<AlertEvent>("AlertEventPage")`.

`paths()`: `let paths = alert_rule_paths(paths);` next to `channel_paths`.

| operationId | Method, path | Summary | Success | Errors |
|---|---|---|---|---|
| `list_alert_rules` | GET `{PROJECT}/alert-rules` | `List alert rules` | `200` `json_named("AlertRulePage", "Every rule of the project, by name.")` | `AUTH_ERRORS` |
| `create_alert_rule` | POST `{PROJECT}/alert-rules` | `Create an alert rule` | `201` `json_of::<AlertRule>("The created rule.")` | `&[400, 401, 403, 404, 409, 429, 503]` |
| `get_alert_rule` | GET `{PROJECT}/alert-rules/{id}` | `Get an alert rule` | `200` `json_of::<AlertRule>("The rule and its state.")` | `&[400, 401, 403, 404, 429]` |
| `update_alert_rule` | PATCH `{PROJECT}/alert-rules/{id}` | `Update an alert rule` | `200` `json_of::<AlertRule>("The updated rule.")` | `&[400, 401, 403, 404, 409, 429, 503]` |
| `delete_alert_rule` | DELETE `{PROJECT}/alert-rules/{id}` | `Delete an alert rule` | `204` `"Deleted, with its events."` | `&[400, 401, 403, 404, 429]` |
| `list_alert_events` | GET `{PROJECT}/alert-rules/{id}/events` | `List a rule's events` | `200` `json_named("AlertEventPage", "Events, newest first, with their deliveries.")` | `QUERY_ERRORS` |

Every operation: tag `"alerts"`, no `.securities(...)`, and `.parameters(Some(vec![project_param()]))`, plus `path_uuid("id", "Alert rule id.")` on the `{id}` paths. `create_alert_rule` has `.request_body(Some(json_body("CreateAlertRuleRequest")))` and `update_alert_rule` `json_body("UpdateAlertRuleRequest")`. The 204 is `ResponseBuilder::new().description("Deleted, with its events.").build()`. `list_alert_events` has `.parameters(Some(with_query::<PageParams>(vec![project_param(), path_uuid("id", "Alert rule id.")])))`; the `{id}` routes `path_uuid("id", "Alert rule id.")`. `create_alert_rule` gets `.description(Some("Needs role member. The filter is checked by the query layer (400 invalid_filter). At most 100 rules per project (409). An enabled rule beyond the org's plan limit is 403 plan_limit."))`; `update_alert_rule` `.description(Some("A new condition, or a change of enabled, resets the state to ok without an event. Re-enabling a rule beyond the org's plan limit is 403 plan_limit."))`.

`problem(code)` already describes 503. In `tests/openapi.rs`:
- `OPERATIONS`: the six entries.
- `expected_errors` chain (it collects only `4xx` keys, so `503` is not listed even though the operations declare it): add `create_alert_rule` and `update_alert_rule` to K5's `create_channel || update_channel` branch (`["400", "401", "403", "404", "409", "429"]`); `get_alert_rule` and `delete_alert_rule` join the `["400", "401", "403", "404", "429"]` branch; `list_alert_events` goes into `QUERY_OR_BODY`; `list_alert_rules` falls to the default.
- `pages`: `("AlertRulePage", "AlertRule")`, `("AlertEventPage", "AlertEvent")` and the two names in the second list.

`just openapi`.

## 5. Fixtures

Static fixtures must be canonical (`tests/api_examples.rs` compares the file with `serde_json::to_string_pretty` of its value plus a newline): two-space indent, one array element per line, `[]` for an empty array. Write the content below, then canonicalize it with

```bash
python3 -c 'import json,sys; p=sys.argv[1]; d=json.load(open(p)); open(p,"w").write(json.dumps(d,indent=2,ensure_ascii=False)+"\n")' fixtures/api/static/alert-rules.json
```

(the key order of the objects below already matches the Rust field order). Do the same for `alert-rule.json` and `alert-events.json`.

Ids: rules `…0510` (firing), `…0511`, `…0512`; events `…0520`–`…0522`; deliveries `…0530`–`…0533` (prefix `01900000-0000-7000-8000-000000000`). Channels are K5's `…0500` (slack) and `…0501` (webhook).

`static/alert-rules.json` (`Page<AlertRule>`, ordered by name):

```json
{
  "data": [
    {
      "id": "01900000-0000-7000-8000-000000000512",
      "name": "Daily cost",
      "enabled": false,
      "condition": {"metric": "cost", "window": "24h", "comparator": "gt", "threshold": 50.0},
      "channel_ids": ["01900000-0000-7000-8000-000000000500"],
      "cooldown_s": 3600,
      "notify_while_firing": false,
      "state": "ok",
      "created_at": "2026-01-01T09:00:00.000000000Z",
      "updated_at": "2026-01-01T09:00:00.000000000Z"
    },
    {
      "id": "01900000-0000-7000-8000-000000000510",
      "name": "High error rate",
      "enabled": true,
      "condition": {"metric": "error_rate", "window": "5m", "filter": "environment = 'production'", "comparator": "gt", "threshold": 0.05, "min_count": 20},
      "channel_ids": ["01900000-0000-7000-8000-000000000500", "01900000-0000-7000-8000-000000000501"],
      "cooldown_s": 3600,
      "notify_while_firing": true,
      "state": "firing",
      "last_value": 0.125,
      "last_count": 48,
      "last_evaluated_at": "2026-01-01T10:06:00.000000000Z",
      "last_fired_at": "2026-01-01T10:05:00.000000000Z",
      "created_at": "2026-01-01T09:00:00.000000000Z",
      "updated_at": "2026-01-01T09:00:00.000000000Z"
    },
    {
      "id": "01900000-0000-7000-8000-000000000511",
      "name": "Low helpfulness",
      "enabled": true,
      "condition": {"metric": "score", "window": "1h", "comparator": "lt", "threshold": 0.7, "min_count": 10, "score_name": "helpfulness", "aggregation": "avg"},
      "channel_ids": [],
      "cooldown_s": 3600,
      "notify_while_firing": false,
      "state": "no_data",
      "last_count": 3,
      "last_evaluated_at": "2026-01-01T10:05:00.000000000Z",
      "created_at": "2026-01-01T09:00:00.000000000Z",
      "updated_at": "2026-01-01T09:00:00.000000000Z"
    }
  ],
  "next_cursor": null
}
```

`Low helpfulness` has no `last_value`: it shows a `no_data` rule whose last evaluation found 3 scores, below `min_count` 10. Keep it that way.

`static/alert-rule.json`: the `High error rate` object above alone (not wrapped in a page).

`static/alert-events.json` (`Page<AlertEvent>` of rule `…0510`, newest first):

```json
{
  "data": [
    {
      "id": "01900000-0000-7000-8000-000000000522",
      "rule_id": "01900000-0000-7000-8000-000000000510",
      "kind": "fired",
      "value": 0.125,
      "count": 48,
      "threshold": 0.05,
      "comparator": "gt",
      "metric": "error_rate",
      "window": "5m",
      "window_from": "2026-01-01T10:00:00.000000000Z",
      "window_to": "2026-01-01T10:05:00.000000000Z",
      "created_at": "2026-01-01T10:05:00.000000000Z",
      "deliveries": [
        {"id": "01900000-0000-7000-8000-000000000532", "channel_id": "01900000-0000-7000-8000-000000000500", "channel_name": "Team Slack", "channel_kind": "slack", "status": "sent", "attempts": 1, "updated_at": "2026-01-01T10:05:00.000000000Z"},
        {"id": "01900000-0000-7000-8000-000000000533", "channel_id": "01900000-0000-7000-8000-000000000501", "channel_name": "Pager webhook", "channel_kind": "webhook", "status": "failed", "attempts": 1, "last_error": "the endpoint answered 500", "updated_at": "2026-01-01T10:05:00.000000000Z"}
      ]
    },
    {
      "id": "01900000-0000-7000-8000-000000000521",
      "rule_id": "01900000-0000-7000-8000-000000000510",
      "kind": "resolved",
      "value": 0.01,
      "count": 51,
      "threshold": 0.05,
      "comparator": "gt",
      "metric": "error_rate",
      "window": "5m",
      "window_from": "2026-01-01T09:35:00.000000000Z",
      "window_to": "2026-01-01T09:40:00.000000000Z",
      "created_at": "2026-01-01T09:40:00.000000000Z",
      "deliveries": [
        {"id": "01900000-0000-7000-8000-000000000531", "channel_id": "01900000-0000-7000-8000-000000000500", "channel_name": "Team Slack", "channel_kind": "slack", "status": "sent", "attempts": 1, "updated_at": "2026-01-01T09:40:00.000000000Z"}
      ]
    },
    {
      "id": "01900000-0000-7000-8000-000000000520",
      "rule_id": "01900000-0000-7000-8000-000000000510",
      "kind": "fired",
      "value": 0.09,
      "count": 44,
      "threshold": 0.05,
      "comparator": "gt",
      "metric": "error_rate",
      "window": "5m",
      "window_from": "2026-01-01T09:15:00.000000000Z",
      "window_to": "2026-01-01T09:20:00.000000000Z",
      "created_at": "2026-01-01T09:20:00.000000000Z",
      "deliveries": [
        {"id": "01900000-0000-7000-8000-000000000530", "channel_id": "01900000-0000-7000-8000-000000000500", "channel_name": "Team Slack", "channel_kind": "slack", "status": "sent", "attempts": 2, "updated_at": "2026-01-01T09:21:00.000000000Z"}
      ]
    }
  ],
  "next_cursor": null
}
```



`api_examples.rs`, static block (use the file's `params(&[…])` helper for path parameters, as the `get_media` entry does):
- `example("list_alert_rules", BTreeMap::new(), BTreeMap::new(), "static/alert-rules.json", "application/json")`;
- `example("get_alert_rule", params(&[("id", "01900000-0000-7000-8000-000000000510")]), BTreeMap::new(), "static/alert-rule.json", "application/json")`;
- `example("list_alert_events", params(&[("id", "01900000-0000-7000-8000-000000000510")]), BTreeMap::new(), "static/alert-events.json", "application/json")`.

Only rule `…0510` has detail and event examples, so in `just web-dev` the other two rules' detail pages show "not found". Accepted; web tests override these endpoints per test.

`just api-examples`. In `tests/api_examples.rs` `generated_files_roundtrip`, add `| "list_alert_rules" | "get_alert_rule" | "list_alert_events"` to the arm that lists the static operations (with `"list_saved_views"` and `"list_channels"`). Import `AlertEvent` and `AlertRule` with `NotificationChannel` from `llmobs_server::api::alerts`. `static_files_match_their_types`: `roundtrip::<Page<AlertRule>>("static/alert-rules.json"); roundtrip::<AlertRule>("static/alert-rule.json"); roundtrip::<Page<AlertEvent>>("static/alert-events.json");`. `roundtrip` compares text: serde_json writes an `f64` such as `50.0` with its `.0`, which the canonicalizing command keeps.

## 6. Tests (`#[cfg(test)] mod tests` in `routes/alert_rules.rs`)

`TestApp::new()`, `seed()`, Ada's cookie; viewer/member helpers as in K5. Insert channels with `alerts::insert_channel` and `NewChannel { config_enc: vec![1, 2, 3], target_hint: "x".to_owned(), … }` (the rule routes never open a config). A second project: `let (other, _) = app.add_project(&seeded, "other", None).await;` (`add_project(&Seeded, &str, Option<u32>) -> (ProjectRow, String)`). Events by SQL: `INSERT INTO alert_events (id, rule_id, kind, threshold, window_from, window_to, details, created_at) VALUES (?, ?, 'fired', 0.1, 0, 1, '{"rule_name":"r","metric":"error_rate","comparator":"gt","window":"5m"}', ?)`; deliveries need a real channel row and `INSERT INTO notification_deliveries (id, event_id, channel_id, status, attempts, created_at, updated_at) VALUES (?, ?, ?, 'sent', 1, ?, ?)`. Base path `format!("/api/v1/projects/{}/alert-rules", seeded.project.id)`.

1. `create_fills_defaults`: POST `{"name":" High errors ","condition":{"metric":"error_rate","window":"5m","comparator":"gt","threshold":0.05}}` → `201`; `name "High errors"`, `condition.min_count 20`, `enabled true`, `cooldown_s 3600`, `notify_while_firing false`, `state "ok"`, `channel_ids []`; GET by id returns the same body.
2. `invalid_condition_is_400_with_fields`: `{"metric":"cost","window":"1h","comparator":"gt","threshold":-1,"min_count":5}` → `400`, fields `condition.threshold` then `condition.min_count`, in this order. A filter of `"  "` → `201` with no `condition.filter`.
3. `invalid_filter_is_invalid_filter`: filter `"error_count >"` → `400`, `code "invalid_filter"`, `errors[0].field == "filter"` with a `position`. The same filter in a PATCH → the same `400`. Filter `"error_count > 0"` → `201`.
4. `unknown_fields_are_refused`: condition with `"group_by":"model"` → `400`.
5. `channels_are_checked`: a channel of another project → `400` field `channel_ids` `unknown channel`; the same id twice → `duplicate channel`; 11 random `Uuid::now_v7()` → `at most 10 channels` (the length check runs first); two real channels → `201` with sorted ids.
6. `cooldown_range`: 299 → `400` field `cooldown_s`; 300 → ok.
7. `update_resets_state_on_condition_change`: create; set the row's state by SQL to `firing` with `last_value 0.9`; PATCH `{"name":"x"}` → `state "firing"`; PATCH `{}` → `200`, the body equal to before (the manual clock does not move, so `updated_at` is equal too); PATCH the same condition → still `firing`; PATCH a new condition → `state "ok"`, no `last_value`; set `firing` again, PATCH `{"enabled":false}` → `state "ok"`, `enabled false`; PATCH `{"channel_ids":[]}` → `channel_ids []`; PATCH `{"cooldown_s":10}` → `400`.
8. `names_are_unique`: duplicate name on create → `409`; renaming a rule to another rule's name → `409`.
9. `delete_and_404s`: DELETE → `204`; GET, PATCH, DELETE again → `404`; GET `…/alert-rules/not-a-uuid` → `400`; a rule of the other project requested through the seeded project's path → `404` for GET, PATCH and DELETE.
10. `events_are_paged_with_deliveries`: insert 3 events by SQL for the rule (times 1, 2, 3) and two deliveries on the newest; GET `…/events?limit=2` → the events at 3 and 2, the newest with 2 deliveries carrying `channel_name`; `next_cursor` present; GET with that cursor → the event at 1 and `next_cursor null`; `?cursor=garbage` → `400` field `cursor`; `?limit=0` → `400`; a rule inserted in the other project, read through the seeded project's path → `404`.
12. `no_plan_limit_by_default`: two enabled rules in the project → both `201`; disable one and re-enable it → `200`.
13. `enforce_quota_counts_enabled_rules`: in the route module's tests, with one enabled rule in the org, `enforce_quota(&app.state, org, Some(1))` is `Err(ApiError::PlanLimit { .. })` and its problem has `status 403`, `code "plan_limit"`; with that rule disabled instead, `Ok(())`.
14. `audit_logs_hold_no_names`: create a rule named `Secret name` with filter `environment = 'x-secret'` inside `assert_logs_clean(&["Secret name", "x-secret"], …)`.
11. `roles`: viewer GETs list, rule and events (`200`) but POST/PATCH/DELETE → `403`; member POST → `201`; `read_key` GET → `200`, POST → `403`.

## Commands

```bash
cargo test -p llmobs-server alert_rules
```

```bash
cargo test -p llmobs-server meta::alerts
```

```bash
just openapi
```

```bash
just api-examples
```

```bash
just check
```

## Done when

- Imports to add beyond the K5 set: in `meta/alerts.rs`, `serde::{Deserialize, Serialize}` and `crate::api::alerts::{AlertEventKind, AlertMetric, AlertWindow, Comparator, DeliveryStatus}`; in `routes/alert_rules.rs`, `serde::{Deserialize, Serialize}` (path and cursor structs), `llmobs_core::ids::OrgId`, `crate::alerts::quota`.
- The six routes answer as above; OpenAPI, schema types, fixtures and manifest are regenerated and committed.
- The 14 route tests, the 2 store tests, the 3 quota tests and `plan_limit_problem`, the OpenAPI and API-example tests and the access suite pass; `just check` is green.
