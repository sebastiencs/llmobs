//! Alert rules, notification channels, events and deliveries (spec 04 § Alerts, spec 07).

use std::collections::HashMap;
use std::fmt;

use llmobs_core::ids::{OrgId, ProjectId};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};
use crate::api::alerts::{
    AlertCondition, AlertEventKind, AlertMetric, AlertState, AlertWindow, ChannelKind, Comparator,
    DeliveryStatus,
};

/// Most channels per project (D-K-5).
pub(crate) const MAX_CHANNELS: i64 = 20;
/// Most rules per project (D-K-5).
pub(crate) const MAX_RULES: i64 = 100;
/// Most channels per rule (D-K-5).
pub(crate) const MAX_RULE_CHANNELS: usize = 10;

/// Conflict details (409 bodies, through `MetaError::Conflict`).
pub(crate) const CHANNEL_NAME_TAKEN: &str = "a channel with this name already exists";
pub(crate) const RULE_NAME_TAKEN: &str = "an alert rule with this name already exists";
pub(crate) const TOO_MANY_CHANNELS: &str = "a project has at most 20 channels";
pub(crate) const TOO_MANY_RULES: &str = "a project has at most 100 alert rules";
pub(crate) const UNKNOWN_CHANNEL: &str = "a channel does not exist in this project";

const CHANNELS: &str = "notification_channels";
const RULES: &str = "alert_rules";
const LINKS: &str = "alert_rule_channels";
const EVENTS: &str = "alert_events";
const DELIVERIES: &str = "notification_deliveries";

/// One stored channel. `config_enc` is sealed; open it with `alerts::channel::open_config`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ChannelRow {
    pub(crate) id: Uuid,
    pub(crate) project_id: ProjectId,
    pub(crate) kind: ChannelKind,
    pub(crate) name: String,
    pub(crate) config_enc: Vec<u8>,
    pub(crate) target_hint: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// Prints the id, kind and name only, so sealed bytes never reach a log.
impl fmt::Debug for ChannelRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelRow")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Columns of a new channel. The caller generates `id` (`Uuid::now_v7()`) to seal the config.
pub(crate) struct NewChannel {
    pub(crate) id: Uuid,
    pub(crate) project_id: ProjectId,
    pub(crate) kind: ChannelKind,
    pub(crate) name: String,
    pub(crate) config_enc: Vec<u8>,
    pub(crate) target_hint: String,
}

/// What [`update_channel`] changes. `None` leaves a column alone.
#[derive(Default)]
pub(crate) struct ChannelPatch {
    pub(crate) name: Option<String>,
    /// The new sealed config and its hint, together.
    pub(crate) config: Option<(Vec<u8>, String)>,
}

/// The newest finished delivery of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastDelivery {
    /// `sent` or `failed`.
    pub(crate) status: String,
    pub(crate) at: i64,
    pub(crate) error: Option<String>,
}

/// One stored rule.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuleRow {
    pub(crate) id: Uuid,
    pub(crate) project_id: ProjectId,
    pub(crate) name: String,
    pub(crate) condition: AlertCondition,
    /// Ordered by channel id.
    pub(crate) channel_ids: Vec<Uuid>,
    pub(crate) enabled: bool,
    pub(crate) cooldown_s: i64,
    pub(crate) notify_while_firing: bool,
    pub(crate) state: AlertState,
    pub(crate) clear_streak: i64,
    pub(crate) last_value: Option<f64>,
    pub(crate) last_count: Option<i64>,
    pub(crate) last_evaluated_at: Option<i64>,
    pub(crate) last_fired_at: Option<i64>,
    pub(crate) last_notified_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) created_by: Option<Uuid>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// Columns of a new rule. `condition` is already normalized; `channel_ids` has no duplicates.
pub(crate) struct NewRule {
    pub(crate) project_id: ProjectId,
    pub(crate) name: String,
    pub(crate) condition: AlertCondition,
    pub(crate) channel_ids: Vec<Uuid>,
    pub(crate) enabled: bool,
    pub(crate) cooldown_s: i64,
    pub(crate) notify_while_firing: bool,
    pub(crate) created_by: Option<Uuid>,
}

/// What [`update_rule`] changes. `None` leaves a column alone.
#[derive(Default)]
pub(crate) struct RulePatch {
    pub(crate) name: Option<String>,
    pub(crate) condition: Option<AlertCondition>,
    pub(crate) channel_ids: Option<Vec<Uuid>>,
    pub(crate) enabled: Option<bool>,
    pub(crate) cooldown_s: Option<i64>,
    pub(crate) notify_while_firing: Option<bool>,
}

#[derive(Debug, sqlx::FromRow)]
struct ChannelRecord {
    id: String,
    project_id: String,
    kind: String,
    name: String,
    config_enc: Vec<u8>,
    target_hint: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct RuleRecord {
    id: String,
    project_id: String,
    name: String,
    metric: String,
    config: String,
    enabled: i64,
    cooldown_s: i64,
    notify_while_firing: i64,
    state: String,
    clear_streak: i64,
    last_value: Option<f64>,
    last_count: Option<i64>,
    last_evaluated_at: Option<i64>,
    last_fired_at: Option<i64>,
    last_notified_at: Option<i64>,
    last_error: Option<String>,
    created_by: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct DeliveryRecord {
    channel_id: String,
    status: String,
    updated_at: i64,
    last_error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct EventRecord {
    id: String,
    rule_id: String,
    kind: String,
    value: Option<f64>,
    count: Option<i64>,
    threshold: f64,
    window_from: i64,
    window_to: i64,
    details: String,
    created_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct EventDeliveryRecord {
    id: String,
    event_id: String,
    channel_id: String,
    channel_name: String,
    channel_kind: String,
    status: String,
    attempts: i64,
    next_attempt_at: Option<i64>,
    last_error: Option<String>,
    created_at: i64,
    updated_at: i64,
}

/// The rule as it was when the event was recorded. Stored as `alert_events.details`, so the
/// history survives later edits (D-K-10).
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

/// Inserts a channel. The project may hold [`MAX_CHANNELS`]; a name taken in the project is
/// [`MetaError::Conflict`] `CHANNEL_NAME_TAKEN`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails or the project is full.
pub(crate) async fn insert_channel(
    conn: &mut SqliteConnection,
    channel: NewChannel,
    now: i64,
) -> Result<ChannelRow, MetaError> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notification_channels WHERE project_id = ?")
            .bind(channel.project_id.to_string())
            .fetch_one(&mut *conn)
            .await?;
    if count >= MAX_CHANNELS {
        return Err(MetaError::Conflict(TOO_MANY_CHANNELS));
    }
    sqlx::query(
        "INSERT INTO notification_channels \
         (id, project_id, kind, name, config_enc, target_hint, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(channel.id.to_string())
    .bind(channel.project_id.to_string())
    .bind(channel.kind.as_str())
    .bind(&channel.name)
    .bind(&channel.config_enc)
    .bind(&channel.target_hint)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, CHANNEL_NAME_TAKEN))?;
    Ok(ChannelRow {
        id: channel.id,
        project_id: channel.project_id,
        kind: channel.kind,
        name: channel.name,
        config_enc: channel.config_enc,
        target_hint: channel.target_hint,
        created_at: now,
        updated_at: now,
    })
}

/// Channels of the project, ordered by name.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_channels(
    pool: &SqlitePool,
    project_id: ProjectId,
) -> Result<Vec<ChannelRow>, MetaError> {
    let records: Vec<ChannelRecord> = sqlx::query_as(
        "SELECT id, project_id, kind, name, config_enc, target_hint, created_at, updated_at \
         FROM notification_channels WHERE project_id = ? ORDER BY name, id",
    )
    .bind(project_id.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(channel_from).collect()
}

/// The channel `id` of the project.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn channel_by_id(
    pool: &SqlitePool,
    project_id: ProjectId,
    id: Uuid,
) -> Result<Option<ChannelRow>, MetaError> {
    let record: Option<ChannelRecord> = sqlx::query_as(
        "SELECT id, project_id, kind, name, config_enc, target_hint, created_at, updated_at \
         FROM notification_channels WHERE project_id = ? AND id = ?",
    )
    .bind(project_id.to_string())
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    record.map(channel_from).transpose()
}

/// Applies the patch and sets `updated_at`. `None` when the channel is not in the project.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or the new name is taken.
pub(crate) async fn update_channel(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    id: Uuid,
    patch: ChannelPatch,
    now: i64,
) -> Result<Option<ChannelRow>, MetaError> {
    let record: Option<ChannelRecord> = sqlx::query_as(
        "SELECT id, project_id, kind, name, config_enc, target_hint, created_at, updated_at \
         FROM notification_channels WHERE project_id = ? AND id = ?",
    )
    .bind(project_id.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(record) = record else {
        return Ok(None);
    };
    let mut row = channel_from(record)?;
    if let Some(name) = patch.name {
        row.name = name;
    }
    if let Some((config_enc, target_hint)) = patch.config {
        row.config_enc = config_enc;
        row.target_hint = target_hint;
    }
    row.updated_at = now;
    sqlx::query(
        "UPDATE notification_channels SET name = ?, config_enc = ?, target_hint = ?, updated_at = ? \
         WHERE project_id = ? AND id = ?",
    )
    .bind(&row.name)
    .bind(&row.config_enc)
    .bind(&row.target_hint)
    .bind(now)
    .bind(project_id.to_string())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, CHANNEL_NAME_TAKEN))?;
    Ok(Some(row))
}

/// Deletes the channel. Its rule links and deliveries go with it by cascade.
///
/// # Errors
///
/// Returns [`MetaError`] when the delete fails.
pub(crate) async fn delete_channel(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query("DELETE FROM notification_channels WHERE project_id = ? AND id = ?")
        .bind(project_id.to_string())
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The newest `sent` or `failed` delivery of each channel of the project that has one.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn last_deliveries(
    pool: &SqlitePool,
    project_id: ProjectId,
) -> Result<HashMap<Uuid, LastDelivery>, MetaError> {
    let records: Vec<DeliveryRecord> = sqlx::query_as(
        "SELECT d.channel_id, d.status, d.updated_at, d.last_error \
         FROM notification_deliveries d \
         JOIN notification_channels c ON c.id = d.channel_id \
         WHERE c.project_id = ? AND d.status IN ('sent', 'failed') \
           AND d.id = (SELECT d2.id FROM notification_deliveries d2 \
                       WHERE d2.channel_id = d.channel_id AND d2.status IN ('sent', 'failed') \
                       ORDER BY d2.updated_at DESC, d2.id DESC LIMIT 1)",
    )
    .bind(project_id.to_string())
    .fetch_all(pool)
    .await?;
    let mut last = HashMap::with_capacity(records.len());
    for record in records {
        let channel = parse_uuid(&record.channel_id, DELIVERIES, "channel_id")?;
        last.insert(
            channel,
            LastDelivery {
                status: record.status,
                at: record.updated_at,
                error: record.last_error,
            },
        );
    }
    Ok(last)
}

/// Inserts a rule in a fresh state (`ok`, no evaluation). Channels must belong to the project.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails, the project is full, a name is taken, or a
/// channel is not in the project.
pub(crate) async fn insert_rule(
    conn: &mut SqliteConnection,
    rule: NewRule,
    now: i64,
) -> Result<RuleRow, MetaError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert_rules WHERE project_id = ?")
        .bind(rule.project_id.to_string())
        .fetch_one(&mut *conn)
        .await?;
    if count >= MAX_RULES {
        return Err(MetaError::Conflict(TOO_MANY_RULES));
    }
    let mut channel_ids = rule.channel_ids;
    channel_ids.sort();
    channel_ids.dedup();
    check_channels(conn, rule.project_id, &channel_ids).await?;

    let id = Uuid::now_v7();
    let config = config_text(&rule.condition);
    sqlx::query(
        "INSERT INTO alert_rules \
         (id, project_id, name, metric, config, enabled, cooldown_s, notify_while_firing, \
          state, clear_streak, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'ok', 0, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(rule.project_id.to_string())
    .bind(&rule.name)
    .bind(rule.condition.metric.as_str())
    .bind(&config)
    .bind(rule.enabled)
    .bind(rule.cooldown_s)
    .bind(rule.notify_while_firing)
    .bind(rule.created_by.map(|user| user.to_string()))
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, RULE_NAME_TAKEN))?;
    for channel in &channel_ids {
        sqlx::query("INSERT INTO alert_rule_channels (rule_id, channel_id) VALUES (?, ?)")
            .bind(id.to_string())
            .bind(channel.to_string())
            .execute(&mut *conn)
            .await?;
    }
    Ok(RuleRow {
        id,
        project_id: rule.project_id,
        name: rule.name,
        condition: rule.condition,
        channel_ids,
        enabled: rule.enabled,
        cooldown_s: rule.cooldown_s,
        notify_while_firing: rule.notify_while_firing,
        state: AlertState::Ok,
        clear_streak: 0,
        last_value: None,
        last_count: None,
        last_evaluated_at: None,
        last_fired_at: None,
        last_notified_at: None,
        last_error: None,
        created_by: rule.created_by,
        created_at: now,
        updated_at: now,
    })
}

/// Rules of the project, ordered by name, each with its channel ids.
///
/// # Errors
///
/// Returns [`MetaError`] when a query fails or a stored value does not decode.
pub(crate) async fn list_rules(
    pool: &SqlitePool,
    project_id: ProjectId,
) -> Result<Vec<RuleRow>, MetaError> {
    let records: Vec<RuleRecord> = sqlx::query_as(
        "SELECT id, project_id, name, metric, config, enabled, cooldown_s, notify_while_firing, \
         state, clear_streak, last_value, last_count, last_evaluated_at, last_fired_at, \
         last_notified_at, last_error, created_by, created_at, updated_at \
         FROM alert_rules WHERE project_id = ? ORDER BY name, id",
    )
    .bind(project_id.to_string())
    .fetch_all(pool)
    .await?;
    let links: Vec<(String, String)> = sqlx::query_as(
        "SELECT rc.rule_id, rc.channel_id FROM alert_rule_channels rc \
         JOIN alert_rules r ON r.id = rc.rule_id \
         WHERE r.project_id = ? ORDER BY rc.rule_id, rc.channel_id",
    )
    .bind(project_id.to_string())
    .fetch_all(pool)
    .await?;
    let mut by_rule: HashMap<String, Vec<Uuid>> = HashMap::new();
    for (rule_id, channel_id) in links {
        let channel = parse_uuid(&channel_id, LINKS, "channel_id")?;
        by_rule.entry(rule_id).or_default().push(channel);
    }
    records
        .into_iter()
        .map(|record| {
            let channel_ids = by_rule.remove(&record.id).unwrap_or_default();
            rule_from(record, channel_ids)
        })
        .collect()
}

/// The rule `id` of the project, with its channel ids.
///
/// # Errors
///
/// Returns [`MetaError`] when a query fails or a stored value does not decode.
pub(crate) async fn rule_by_id(
    pool: &SqlitePool,
    project_id: ProjectId,
    id: Uuid,
) -> Result<Option<RuleRow>, MetaError> {
    let mut conn = pool.acquire().await?;
    rule_on(&mut conn, project_id, id).await
}

/// The channel ids linked to `rule_id`, ordered by id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or an id does not decode.
pub(crate) async fn rule_channel_ids(
    pool: &SqlitePool,
    rule_id: Uuid,
) -> Result<Vec<Uuid>, MetaError> {
    let mut conn = pool.acquire().await?;
    channel_ids_on(&mut conn, rule_id).await
}

/// Applies the patch and sets `updated_at`. `None` when the rule is not in the project.
///
/// A new condition, or a change of `enabled`, resets the state to `ok` (D-K-4). The
/// last fired and notified times are kept.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails, the new name is taken, or a channel is not
/// in the project.
pub(crate) async fn update_rule(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    id: Uuid,
    patch: RulePatch,
    now: i64,
) -> Result<Option<RuleRow>, MetaError> {
    let Some(mut row) = rule_on(conn, project_id, id).await? else {
        return Ok(None);
    };
    if let Some(name) = patch.name {
        row.name = name;
    }
    let mut reset = false;
    if let Some(condition) = patch.condition
        && condition != row.condition
    {
        row.condition = condition;
        reset = true;
    }
    if let Some(enabled) = patch.enabled
        && enabled != row.enabled
    {
        row.enabled = enabled;
        reset = true;
    }
    if reset {
        row.state = AlertState::Ok;
        row.clear_streak = 0;
        row.last_value = None;
        row.last_count = None;
        row.last_evaluated_at = None;
        row.last_error = None;
    }
    if let Some(cooldown_s) = patch.cooldown_s {
        row.cooldown_s = cooldown_s;
    }
    if let Some(notify_while_firing) = patch.notify_while_firing {
        row.notify_while_firing = notify_while_firing;
    }
    if let Some(mut channel_ids) = patch.channel_ids {
        channel_ids.sort();
        channel_ids.dedup();
        check_channels(conn, project_id, &channel_ids).await?;
        sqlx::query("DELETE FROM alert_rule_channels WHERE rule_id = ?")
            .bind(id.to_string())
            .execute(&mut *conn)
            .await?;
        for channel in &channel_ids {
            sqlx::query("INSERT INTO alert_rule_channels (rule_id, channel_id) VALUES (?, ?)")
                .bind(id.to_string())
                .bind(channel.to_string())
                .execute(&mut *conn)
                .await?;
        }
    }
    row.updated_at = now;
    sqlx::query(
        "UPDATE alert_rules SET name = ?, metric = ?, config = ?, enabled = ?, cooldown_s = ?, \
         notify_while_firing = ?, state = ?, clear_streak = ?, last_value = ?, last_count = ?, \
         last_evaluated_at = ?, last_error = ?, updated_at = ? \
         WHERE project_id = ? AND id = ?",
    )
    .bind(&row.name)
    .bind(row.condition.metric.as_str())
    .bind(config_text(&row.condition))
    .bind(row.enabled)
    .bind(row.cooldown_s)
    .bind(row.notify_while_firing)
    .bind(row.state.as_str())
    .bind(row.clear_streak)
    .bind(row.last_value)
    .bind(row.last_count)
    .bind(row.last_evaluated_at)
    .bind(&row.last_error)
    .bind(now)
    .bind(project_id.to_string())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, RULE_NAME_TAKEN))?;
    rule_on(conn, project_id, id).await
}

/// Deletes the rule. Its events, deliveries and links go with it by cascade.
///
/// # Errors
///
/// Returns [`MetaError`] when the delete fails.
pub(crate) async fn delete_rule(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query("DELETE FROM alert_rules WHERE project_id = ? AND id = ?")
        .bind(project_id.to_string())
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Enabled rules of active projects in the org (D178's per-org limit, D-K-14).
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails.
pub(crate) async fn count_enabled_org_rules(
    pool: &SqlitePool,
    org_id: OrgId,
) -> Result<i64, MetaError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM alert_rules r \
         JOIN projects p ON p.id = r.project_id \
         WHERE p.org_id = ? AND p.state = 'active' AND r.enabled = 1",
    )
    .bind(org_id.to_string())
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Events of `rule_id`, newest first (`created_at DESC, id DESC`), strictly before `before`
/// when given, at most `limit` rows.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_events(
    pool: &SqlitePool,
    rule_id: Uuid,
    before: Option<(i64, Uuid)>,
    limit: i64,
) -> Result<Vec<EventRow>, MetaError> {
    let before_at = before.map(|cursor| cursor.0);
    let before_id = before.map(|cursor| cursor.1.to_string());
    let records: Vec<EventRecord> = sqlx::query_as(
        "SELECT id, rule_id, kind, value, count, threshold, window_from, window_to, details, \
         created_at FROM alert_events WHERE rule_id = ?1 \
         AND (?2 IS NULL OR created_at < ?2 OR (created_at = ?2 AND id < ?3)) \
         ORDER BY created_at DESC, id DESC LIMIT ?4",
    )
    .bind(rule_id.to_string())
    .bind(before_at)
    .bind(before_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    records.iter().map(event_from).collect()
}

/// Deliveries of the given events, grouped in the order of `event_ids`, then by `created_at`,
/// then `id`. Runs one query per event: a page holds at most 1 000 events.
///
/// # Errors
///
/// Returns [`MetaError`] when a query fails or a stored value does not decode.
pub(crate) async fn deliveries_for_events(
    pool: &SqlitePool,
    event_ids: &[Uuid],
) -> Result<Vec<DeliveryRow>, MetaError> {
    let mut rows = Vec::new();
    for event_id in event_ids {
        let records: Vec<EventDeliveryRecord> = sqlx::query_as(
            "SELECT d.id, d.event_id, d.channel_id, c.name AS channel_name, c.kind AS channel_kind, \
             d.status, d.attempts, d.next_attempt_at, d.last_error, d.created_at, d.updated_at \
             FROM notification_deliveries d JOIN notification_channels c ON c.id = d.channel_id \
             WHERE d.event_id = ? ORDER BY d.created_at, d.id",
        )
        .bind(event_id.to_string())
        .fetch_all(pool)
        .await?;
        for record in records {
            rows.push(delivery_from(record)?);
        }
    }
    Ok(rows)
}

/// The rule `id` of the project on `conn`, with its channel ids.
async fn rule_on(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    id: Uuid,
) -> Result<Option<RuleRow>, MetaError> {
    let record: Option<RuleRecord> = sqlx::query_as(
        "SELECT id, project_id, name, metric, config, enabled, cooldown_s, notify_while_firing, \
         state, clear_streak, last_value, last_count, last_evaluated_at, last_fired_at, \
         last_notified_at, last_error, created_by, created_at, updated_at \
         FROM alert_rules WHERE project_id = ? AND id = ?",
    )
    .bind(project_id.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(record) = record else {
        return Ok(None);
    };
    let channel_ids = channel_ids_on(conn, id).await?;
    rule_from(record, channel_ids).map(Some)
}

/// The channel ids linked to `rule_id` on `conn`, ordered by id.
async fn channel_ids_on(
    conn: &mut SqliteConnection,
    rule_id: Uuid,
) -> Result<Vec<Uuid>, MetaError> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT channel_id FROM alert_rule_channels WHERE rule_id = ? ORDER BY channel_id",
    )
    .bind(rule_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    texts
        .iter()
        .map(|text| parse_uuid(text, LINKS, "channel_id"))
        .collect()
}

/// Fails with `UNKNOWN_CHANNEL` when any id is not a channel of the project.
async fn check_channels(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    ids: &[Uuid],
) -> Result<(), MetaError> {
    for id in ids {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notification_channels WHERE project_id = ? AND id = ?",
        )
        .bind(project_id.to_string())
        .bind(id.to_string())
        .fetch_one(&mut *conn)
        .await?;
        if count == 0 {
            return Err(MetaError::Conflict(UNKNOWN_CHANNEL));
        }
    }
    Ok(())
}

/// The stored `config` text of a condition. Serializing an `AlertCondition` cannot fail: it
/// holds only enums, numbers and strings.
#[allow(clippy::expect_used)]
fn config_text(condition: &AlertCondition) -> String {
    serde_json::to_string(condition).expect("alert condition serializes")
}

fn corrupt(table: &'static str, column: &'static str) -> MetaError {
    MetaError::Corrupt { table, column }
}

fn parse_uuid(text: &str, table: &'static str, column: &'static str) -> Result<Uuid, MetaError> {
    text.parse().map_err(|_| corrupt(table, column))
}

fn channel_from(record: ChannelRecord) -> Result<ChannelRow, MetaError> {
    let kind = ChannelKind::parse(&record.kind).ok_or_else(|| corrupt(CHANNELS, "kind"))?;
    Ok(ChannelRow {
        id: parse_uuid(&record.id, CHANNELS, "id")?,
        project_id: record
            .project_id
            .parse()
            .map_err(|_| corrupt(CHANNELS, "project_id"))?,
        kind,
        name: record.name,
        config_enc: record.config_enc,
        target_hint: record.target_hint,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn rule_from(record: RuleRecord, channel_ids: Vec<Uuid>) -> Result<RuleRow, MetaError> {
    let condition: AlertCondition =
        serde_json::from_str(&record.config).map_err(|_| corrupt(RULES, "config"))?;
    if record.metric != condition.metric.as_str() {
        return Err(corrupt(RULES, "metric"));
    }
    let state = AlertState::parse(&record.state).ok_or_else(|| corrupt(RULES, "state"))?;
    Ok(RuleRow {
        id: parse_uuid(&record.id, RULES, "id")?,
        project_id: record
            .project_id
            .parse()
            .map_err(|_| corrupt(RULES, "project_id"))?,
        name: record.name,
        condition,
        channel_ids,
        enabled: record.enabled != 0,
        cooldown_s: record.cooldown_s,
        notify_while_firing: record.notify_while_firing != 0,
        state,
        clear_streak: record.clear_streak,
        last_value: record.last_value,
        last_count: record.last_count,
        last_evaluated_at: record.last_evaluated_at,
        last_fired_at: record.last_fired_at,
        last_notified_at: record.last_notified_at,
        last_error: record.last_error,
        created_by: record
            .created_by
            .as_deref()
            .map(|text| parse_uuid(text, RULES, "created_by"))
            .transpose()?,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn event_from(record: &EventRecord) -> Result<EventRow, MetaError> {
    let kind = AlertEventKind::parse(&record.kind).ok_or_else(|| corrupt(EVENTS, "kind"))?;
    let details: EventDetails =
        serde_json::from_str(&record.details).map_err(|_| corrupt(EVENTS, "details"))?;
    Ok(EventRow {
        id: parse_uuid(&record.id, EVENTS, "id")?,
        rule_id: parse_uuid(&record.rule_id, EVENTS, "rule_id")?,
        kind,
        value: record.value,
        count: record.count,
        threshold: record.threshold,
        window_from: record.window_from,
        window_to: record.window_to,
        details,
        created_at: record.created_at,
    })
}

fn delivery_from(record: EventDeliveryRecord) -> Result<DeliveryRow, MetaError> {
    let channel_kind =
        ChannelKind::parse(&record.channel_kind).ok_or_else(|| corrupt(CHANNELS, "kind"))?;
    let status =
        DeliveryStatus::parse(&record.status).ok_or_else(|| corrupt(DELIVERIES, "status"))?;
    Ok(DeliveryRow {
        id: parse_uuid(&record.id, DELIVERIES, "id")?,
        event_id: parse_uuid(&record.event_id, DELIVERIES, "event_id")?,
        channel_id: parse_uuid(&record.channel_id, DELIVERIES, "channel_id")?,
        channel_name: record.channel_name,
        channel_kind,
        status,
        attempts: record.attempts,
        next_attempt_at: record.next_attempt_at,
        last_error: record.last_error,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::{MetaDb, test_db};

    fn new_channel(project_id: ProjectId, name: &str) -> NewChannel {
        NewChannel {
            id: Uuid::now_v7(),
            project_id,
            kind: ChannelKind::Webhook,
            name: name.to_owned(),
            config_enc: vec![1, 2, 3],
            target_hint: "hooks.example.com".to_owned(),
        }
    }

    fn condition(threshold: f64) -> AlertCondition {
        AlertCondition {
            metric: AlertMetric::ErrorRate,
            window: AlertWindow::M5,
            filter: None,
            comparator: Comparator::Gt,
            threshold,
            min_count: Some(20),
            target: None,
            score_name: None,
            aggregation: None,
        }
    }

    fn new_rule(project_id: ProjectId, name: &str, channel_ids: Vec<Uuid>) -> NewRule {
        NewRule {
            project_id,
            name: name.to_owned(),
            condition: condition(0.1),
            channel_ids,
            enabled: true,
            cooldown_s: 3600,
            notify_while_firing: false,
            created_by: None,
        }
    }

    /// Two projects, `A` and `B`, in one org.
    async fn two_projects(db: &MetaDb) -> (ProjectId, ProjectId) {
        db.write(|conn| {
            Box::pin(async move {
                let org = insert_org(conn, "O", "o", 1).await?;
                let a = insert_project(conn, org.id, "A", "a", None, 1).await?;
                let b = insert_project(conn, org.id, "B", "b", None, 1).await?;
                Ok((a.id, b.id))
            })
        })
        .await
        .unwrap()
    }

    async fn add_channel(db: &MetaDb, channel: NewChannel) -> Result<ChannelRow, MetaError> {
        db.write(move |conn| Box::pin(async move { insert_channel(conn, channel, 1).await }))
            .await
    }

    async fn add_rule(db: &MetaDb, rule: NewRule) -> Result<RuleRow, MetaError> {
        db.write(move |conn| Box::pin(async move { insert_rule(conn, rule, 1).await }))
            .await
    }

    /// Puts a rule into `firing` with a clear streak, a value, an error and a fired time.
    async fn set_firing(db: &MetaDb, id: Uuid) {
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query(
                    "UPDATE alert_rules SET state = 'firing', clear_streak = 1, last_value = 0.9, \
                     last_evaluated_at = 5, last_error = 'x', last_fired_at = 4 WHERE id = ?",
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

    async fn insert_delivery(
        conn: &mut SqliteConnection,
        event: &str,
        channel: &str,
        status: &str,
        at: i64,
        error: Option<&str>,
    ) -> Result<(), MetaError> {
        sqlx::query(
            "INSERT INTO notification_deliveries \
             (id, event_id, channel_id, status, last_error, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(event)
        .bind(channel)
        .bind(status)
        .bind(error)
        .bind(at)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Inserts an event of `rule` at `created_at` by SQL, and returns its id.
    async fn insert_event(db: &MetaDb, rule: Uuid, created_at: i64) -> Uuid {
        let id = Uuid::now_v7();
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO alert_events (id, rule_id, kind, threshold, window_from, window_to, \
                     details, created_at) VALUES (?, ?, 'fired', 0.1, 0, 1, \
                     '{\"rule_name\":\"r\",\"metric\":\"error_rate\",\"comparator\":\"gt\",\"window\":\"5m\"}', ?)",
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

    #[tokio::test]
    async fn events_page_newest_first() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        let rule = add_rule(&db, new_rule(a, "r", Vec::new()))
            .await
            .unwrap()
            .id;
        let mut ids = Vec::new();
        for created_at in [10, 20, 30, 40, 50] {
            ids.push(insert_event(&db, rule, created_at).await);
        }

        let first = list_events(db.reader(), rule, None, 2).await.unwrap();
        let first_ids: Vec<Uuid> = first.iter().map(|event| event.id).collect();
        assert_eq!(first_ids, [ids[4], ids[3]]);
        assert_eq!(first[0].kind, AlertEventKind::Fired);
        assert_eq!(first[0].details.rule_name, "r");
        assert_eq!(first[0].details.metric, AlertMetric::ErrorRate);
        assert_eq!(first[0].details.window, AlertWindow::M5);
        assert_eq!(first[0].details.comparator, Comparator::Gt);
        assert!(first[0].details.filter.is_none());

        let second = list_events(db.reader(), rule, Some((40, ids[3])), 2)
            .await
            .unwrap();
        let second_ids: Vec<Uuid> = second.iter().map(|event| event.id).collect();
        assert_eq!(second_ids, [ids[2], ids[1]]);
    }

    #[tokio::test]
    async fn deliveries_join_channel_names() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        let slack = add_channel(&db, new_channel(a, "Slack")).await.unwrap().id;
        let mail = add_channel(&db, new_channel(a, "Mail")).await.unwrap().id;
        let rule = add_rule(&db, new_rule(a, "r", Vec::new()))
            .await
            .unwrap()
            .id;
        let first = insert_event(&db, rule, 10).await;
        let second = insert_event(&db, rule, 20).await;
        let (late, early, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        db.write(move |conn| {
            Box::pin(async move {
                for (id, event, channel, status, created_at) in [
                    (late, first, mail, "failed", 9),
                    (early, first, slack, "sent", 3),
                    (other, second, slack, "sent", 1),
                ] {
                    sqlx::query(
                        "INSERT INTO notification_deliveries (id, event_id, channel_id, status, \
                         attempts, created_at, updated_at) VALUES (?, ?, ?, ?, 1, ?, ?)",
                    )
                    .bind(id.to_string())
                    .bind(event.to_string())
                    .bind(channel.to_string())
                    .bind(status)
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

        let rows = deliveries_for_events(db.reader(), &[second, first])
            .await
            .unwrap();
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        assert_eq!(ids, [other, early, late]);
        assert_eq!(rows[1].channel_name, "Slack");
        assert_eq!(rows[1].channel_kind, ChannelKind::Webhook);
        assert_eq!(rows[1].status, DeliveryStatus::Sent);
        assert_eq!(rows[2].channel_name, "Mail");
        assert_eq!(rows[2].status, DeliveryStatus::Failed);
        assert_eq!(rows[2].event_id, first);

        assert!(
            deliveries_for_events(db.reader(), &[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn channel_crud() {
        let (_dir, db) = test_db().await;
        let (a, b) = two_projects(&db).await;
        let created = add_channel(&db, new_channel(a, "ops")).await.unwrap();
        add_channel(&db, new_channel(a, "alpha")).await.unwrap();
        let id = created.id;

        assert_eq!(
            channel_by_id(db.reader(), a, id).await.unwrap(),
            Some(created.clone())
        );
        let names: Vec<String> = list_channels(db.reader(), a)
            .await
            .unwrap()
            .into_iter()
            .map(|channel| channel.name)
            .collect();
        assert_eq!(names, ["alpha", "ops"]);

        let updated = db
            .write(move |conn| {
                Box::pin(async move {
                    update_channel(
                        conn,
                        a,
                        id,
                        ChannelPatch {
                            name: Some("pager".to_owned()),
                            config: Some((vec![9], "h2".to_owned())),
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "pager");
        assert_eq!(updated.config_enc, vec![9]);
        assert_eq!(updated.target_hint, "h2");
        assert_eq!(updated.created_at, 1);
        assert_eq!(updated.updated_at, 2);

        let stored: Vec<String> =
            sqlx::query_scalar("SELECT id FROM notification_channels WHERE name = 'pager'")
                .fetch_all(db.reader())
                .await
                .unwrap();
        assert_eq!(stored, [id.to_string()]);
        assert!(channel_by_id(db.reader(), b, id).await.unwrap().is_none());
        let other = db
            .write(move |conn| {
                Box::pin(
                    async move { update_channel(conn, b, id, ChannelPatch::default(), 3).await },
                )
            })
            .await
            .unwrap();
        assert!(other.is_none());

        let deleted = db
            .write(move |conn| Box::pin(async move { delete_channel(conn, a, id).await }))
            .await
            .unwrap();
        assert!(deleted);
        let again = db
            .write(move |conn| Box::pin(async move { delete_channel(conn, a, id).await }))
            .await
            .unwrap();
        assert!(!again);
    }

    #[tokio::test]
    async fn channel_names_are_unique_per_project() {
        let (_dir, db) = test_db().await;
        let (a, b) = two_projects(&db).await;
        add_channel(&db, new_channel(a, "ops")).await.unwrap();
        let error = add_channel(&db, new_channel(a, "ops")).await.unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(CHANNEL_NAME_TAKEN)),
            "{error}"
        );
        add_channel(&db, new_channel(b, "ops")).await.unwrap();
    }

    #[tokio::test]
    async fn channel_limit() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        db.write(move |conn| {
            Box::pin(async move {
                for index in 0..MAX_CHANNELS {
                    insert_channel(conn, new_channel(a, &format!("c{index}")), 1).await?;
                }
                Ok(())
            })
        })
        .await
        .unwrap();
        let error = add_channel(&db, new_channel(a, "extra")).await.unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(TOO_MANY_CHANNELS)),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rule_crud_and_channels() {
        let (_dir, db) = test_db().await;
        let (a, b) = two_projects(&db).await;
        let c1 = add_channel(&db, new_channel(a, "one")).await.unwrap().id;
        let c2 = add_channel(&db, new_channel(a, "two")).await.unwrap().id;
        let rule = add_rule(&db, new_rule(a, "errors", vec![c2, c1, c1]))
            .await
            .unwrap();
        let id = rule.id;

        let mut expected = vec![c1, c2];
        expected.sort();
        let loaded = rule_by_id(db.reader(), a, id).await.unwrap().unwrap();
        assert_eq!(loaded.channel_ids, expected);
        assert_eq!(loaded.condition, condition(0.1));
        assert_eq!(loaded.state, AlertState::Ok);
        assert_eq!(loaded.clear_streak, 0);
        assert_eq!(loaded, rule);
        assert_eq!(rule_channel_ids(db.reader(), id).await.unwrap(), expected);

        let updated = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            channel_ids: Some(vec![c1]),
                            ..RulePatch::default()
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.channel_ids, vec![c1]);
        assert_eq!(updated.updated_at, 2);

        assert!(rule_by_id(db.reader(), b, id).await.unwrap().is_none());
        let other = db
            .write(move |conn| {
                Box::pin(async move { update_rule(conn, b, id, RulePatch::default(), 3).await })
            })
            .await
            .unwrap();
        assert!(other.is_none());

        let deleted = db
            .write(move |conn| Box::pin(async move { delete_rule(conn, a, id).await }))
            .await
            .unwrap();
        assert!(deleted);
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert_rule_channels")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(links, 0);
    }

    #[tokio::test]
    async fn rule_rejects_foreign_channels() {
        let (_dir, db) = test_db().await;
        let (a, b) = two_projects(&db).await;
        let foreign = add_channel(&db, new_channel(b, "theirs")).await.unwrap().id;
        let error = add_rule(&db, new_rule(a, "r", vec![foreign]))
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(UNKNOWN_CHANNEL)),
            "{error}"
        );

        let mine = add_channel(&db, new_channel(a, "mine")).await.unwrap().id;
        let id = add_rule(&db, new_rule(a, "r", vec![mine]))
            .await
            .unwrap()
            .id;
        let error = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            channel_ids: Some(vec![foreign]),
                            ..RulePatch::default()
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(UNKNOWN_CHANNEL)),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rule_limit_and_names() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        add_rule(&db, new_rule(a, "r0", vec![])).await.unwrap();
        let error = add_rule(&db, new_rule(a, "r0", vec![])).await.unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(RULE_NAME_TAKEN)),
            "{error}"
        );
        for index in 1..MAX_RULES {
            add_rule(&db, new_rule(a, &format!("r{index}"), vec![]))
                .await
                .unwrap();
        }
        let error = add_rule(&db, new_rule(a, "last", vec![]))
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(TOO_MANY_RULES)),
            "{error}"
        );
    }

    #[tokio::test]
    async fn condition_change_resets_state() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        let id = add_rule(&db, new_rule(a, "r", vec![])).await.unwrap().id;
        set_firing(&db, id).await;

        let renamed = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            name: Some("renamed".to_owned()),
                            ..RulePatch::default()
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.state, AlertState::Firing);
        assert_eq!(renamed.clear_streak, 1);

        let same = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            condition: Some(condition(0.1)),
                            ..RulePatch::default()
                        },
                        3,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(same.state, AlertState::Firing);

        let changed = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            condition: Some(condition(0.2)),
                            ..RulePatch::default()
                        },
                        4,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(changed.state, AlertState::Ok);
        assert_eq!(changed.clear_streak, 0);
        assert_eq!(changed.last_value, None);
        assert_eq!(changed.last_count, None);
        assert_eq!(changed.last_evaluated_at, None);
        assert_eq!(changed.last_error, None);
        assert_eq!(changed.last_fired_at, Some(4));
        assert_eq!(changed.condition.threshold, 0.2);
    }

    #[tokio::test]
    async fn disabling_resets_state() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        let id = add_rule(&db, new_rule(a, "r", vec![])).await.unwrap().id;
        set_firing(&db, id).await;

        let disabled = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            enabled: Some(false),
                            ..RulePatch::default()
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.state, AlertState::Ok);
        assert_eq!(disabled.clear_streak, 0);
        assert_eq!(disabled.last_value, None);
        assert_eq!(disabled.last_fired_at, Some(4));

        let enabled = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        id,
                        RulePatch {
                            enabled: Some(true),
                            ..RulePatch::default()
                        },
                        3,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert!(enabled.enabled);
        assert_eq!(enabled.state, AlertState::Ok);
    }

    #[tokio::test]
    async fn deleting_a_channel_unlinks_it() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        let channel = add_channel(&db, new_channel(a, "c")).await.unwrap().id;
        let rule = add_rule(&db, new_rule(a, "r", vec![channel]))
            .await
            .unwrap();
        let deleted = db
            .write(move |conn| Box::pin(async move { delete_channel(conn, a, channel).await }))
            .await
            .unwrap();
        assert!(deleted);
        let loaded = rule_by_id(db.reader(), a, rule.id).await.unwrap().unwrap();
        assert!(loaded.channel_ids.is_empty());
    }

    #[tokio::test]
    async fn last_deliveries_picks_the_newest_finished() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        let busy = add_channel(&db, new_channel(a, "busy")).await.unwrap();
        let quiet = add_channel(&db, new_channel(a, "quiet")).await.unwrap();
        let rule = add_rule(&db, new_rule(a, "r", vec![busy.id, quiet.id]))
            .await
            .unwrap();
        let event = Uuid::now_v7().to_string();
        let rule_text = rule.id.to_string();
        let (busy_text, quiet_text) = (busy.id.to_string(), quiet.id.to_string());
        let event_for_write = event.clone();
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO alert_events \
                     (id, rule_id, kind, threshold, window_from, window_to, details, created_at) \
                     VALUES (?, ?, 'fired', 1.0, 0, 1, '{}', 1)",
                )
                .bind(&event_for_write)
                .bind(&rule_text)
                .execute(&mut *conn)
                .await?;
                insert_delivery(conn, &event_for_write, &busy_text, "pending", 30, None).await?;
                insert_delivery(
                    conn,
                    &event_for_write,
                    &busy_text,
                    "failed",
                    10,
                    Some("timeout"),
                )
                .await?;
                insert_delivery(conn, &event_for_write, &busy_text, "sent", 20, None).await?;
                insert_delivery(conn, &event_for_write, &quiet_text, "pending", 40, None).await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let last = last_deliveries(db.reader(), a).await.unwrap();
        assert_eq!(
            last.get(&busy.id),
            Some(&LastDelivery {
                status: "sent".to_owned(),
                at: 20,
                error: None,
            })
        );
        assert!(!last.contains_key(&quiet.id));
    }

    #[tokio::test]
    async fn corrupt_rows_are_reported() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        add_rule(&db, new_rule(a, "r", vec![])).await.unwrap();
        db.write(|conn| {
            Box::pin(async move {
                sqlx::query("UPDATE alert_rules SET state = 'weird'")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let error = list_rules(db.reader(), a).await.unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Corrupt {
                    table: "alert_rules",
                    column: "state",
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn enabled_rules_are_counted_per_org() {
        let (_dir, db) = test_db().await;
        let (org, a, b, c) = db
            .write(|conn| {
                Box::pin(async move {
                    let org = insert_org(conn, "O", "o", 1).await?;
                    let a = insert_project(conn, org.id, "A", "a", None, 1).await?;
                    let b = insert_project(conn, org.id, "B", "b", None, 1).await?;
                    let other = insert_org(conn, "P", "p", 1).await?;
                    let c = insert_project(conn, other.id, "C", "c", None, 1).await?;
                    Ok((org.id, a.id, b.id, c.id))
                })
            })
            .await
            .unwrap();
        add_rule(&db, new_rule(a, "a-on", vec![])).await.unwrap();
        let mut off = new_rule(a, "a-off", vec![]);
        off.enabled = false;
        add_rule(&db, off).await.unwrap();
        add_rule(&db, new_rule(b, "b-on", vec![])).await.unwrap();
        add_rule(&db, new_rule(c, "c-on", vec![])).await.unwrap();
        assert_eq!(count_enabled_org_rules(db.reader(), org).await.unwrap(), 2);

        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("UPDATE projects SET state = 'deleting' WHERE id = ?")
                    .bind(b.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        assert_eq!(count_enabled_org_rules(db.reader(), org).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn renames_to_a_taken_name_conflict() {
        let (_dir, db) = test_db().await;
        let (a, _) = two_projects(&db).await;
        add_channel(&db, new_channel(a, "x")).await.unwrap();
        let y = add_channel(&db, new_channel(a, "y")).await.unwrap().id;
        let error = db
            .write(move |conn| {
                Box::pin(async move {
                    update_channel(
                        conn,
                        a,
                        y,
                        ChannelPatch {
                            name: Some("x".to_owned()),
                            config: None,
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(CHANNEL_NAME_TAKEN)),
            "{error}"
        );

        add_rule(&db, new_rule(a, "one", vec![])).await.unwrap();
        let two = add_rule(&db, new_rule(a, "two", vec![])).await.unwrap().id;
        let error = db
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        a,
                        two,
                        RulePatch {
                            name: Some("one".to_owned()),
                            ..RulePatch::default()
                        },
                        2,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(RULE_NAME_TAKEN)),
            "{error}"
        );
    }
}
