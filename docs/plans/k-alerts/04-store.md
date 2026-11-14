# K4 — Alert tables, the `meta::alerts` store, `ManageAlerts`, sealed channel configs

**Commit subject:** `server: store alert rules and notification channels in meta.db`

Adds the five alert tables, the store functions for channels and rules (events and deliveries get theirs in K7 and K8), the `ProjectPerm::ManageAlerts` permission, and sealing of a channel's config with track I's `SecretBox`. No route yet.

**Prerequisite:** track I M1 is merged (README § Prerequisite). Check it first:

```bash
grep -n "pub(crate) struct SecretBox" crates/server/src/crypto.rs
```

```bash
grep -n "pub(crate) const ENCRYPTED_COLUMNS" crates/server/src/crypto.rs
```

Each must print one line. Otherwise STOP and report.

## Read first

- README § Design → Model, § Migration number, D-K-4, D-K-5, D-K-6, D-K-9.
- `crates/server/src/meta/mod.rs`: `MetaDb::write`, `MetaDb::reader`, `MetaError`, `unique_or`, `test_db`, and the tests `open_twice_is_idempotent`, `open_existing_checks_the_file_and_the_version`, `schema_matches_the_snapshot`.
- `crates/server/src/meta/api_keys.rs` (house style: a row struct, a private `sqlx::FromRow` record, a `row_from` decoder, `MetaError::Corrupt`), `crates/server/src/meta/projects.rs` (`update_project` and its patch struct; its tests, which create an org and projects with `insert_org` and `insert_project`).
- `crates/server/src/admin.rs` test `schema_mismatch_names_the_versions`.
- `crates/server/src/crypto.rs` (I M1): `SecretBox::seal`, `SecretBox::open`, `Aad`, `CryptoError`, `ENCRYPTED_COLUMNS`, test `encrypted_columns_cover_the_schema`.
- `crates/server/src/auth/roles.rs`: `ProjectPerm`, `min_role`, `key_scope`, test `permissions_follow_the_spec_table`.
- K3's `crate::api::alerts` and `crate::alerts::channel`.

## Files

| File | Change |
|---|---|
| `crates/server/migrations/meta/NNNN_alerts.sql` | new (README § Migration number) |
| `crates/server/src/meta/alerts.rs` | new |
| `crates/server/src/meta/mod.rs` | `pub(crate) mod alerts;` (first in the module list); version numbers in two tests |
| `crates/server/src/admin.rs` | version numbers in one test |
| `crates/server/tests/snapshots/meta_schema.snap` | regenerated |
| `crates/server/src/crypto.rs` | one row in `ENCRYPTED_COLUMNS` |
| `crates/server/src/alerts/channel.rs` | `seal_config`, `open_config` |
| `crates/server/src/auth/roles.rs` | `ProjectPerm::ManageAlerts` |

## 1. Migration

Let `P` be the highest migration number in `crates/server/migrations/meta/` today and `N = P + 1`. Create `crates/server/migrations/meta/<N as 4 digits>_alerts.sql` with exactly:

```sql
CREATE TABLE notification_channels (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  config_enc BLOB NOT NULL,
  target_hint TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX notification_channels_name ON notification_channels(project_id, name);

CREATE TABLE alert_rules (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  metric TEXT NOT NULL,
  config TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  cooldown_s INTEGER NOT NULL DEFAULT 3600,
  notify_while_firing INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'ok',
  clear_streak INTEGER NOT NULL DEFAULT 0,
  last_value REAL,
  last_count INTEGER,
  last_evaluated_at INTEGER,
  last_fired_at INTEGER,
  last_notified_at INTEGER,
  last_error TEXT,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX alert_rules_name ON alert_rules(project_id, name);
CREATE INDEX alert_rules_due ON alert_rules(enabled, last_evaluated_at);

CREATE TABLE alert_rule_channels (
  rule_id TEXT NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
  channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  PRIMARY KEY (rule_id, channel_id)
);
CREATE INDEX alert_rule_channels_channel ON alert_rule_channels(channel_id);

CREATE TABLE alert_events (
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  value REAL,
  count INTEGER,
  threshold REAL NOT NULL,
  window_from INTEGER NOT NULL,
  window_to INTEGER NOT NULL,
  details TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX alert_events_rule ON alert_events(rule_id, created_at);
CREATE INDEX alert_events_created ON alert_events(created_at);

CREATE TABLE notification_deliveries (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES alert_events(id) ON DELETE CASCADE,
  channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX notification_deliveries_due ON notification_deliveries(status, next_attempt_at);
CREATE INDEX notification_deliveries_event ON notification_deliveries(event_id);
CREATE INDEX notification_deliveries_channel ON notification_deliveries(channel_id, updated_at);
```

Column meanings (they also go into spec 04, already done by the planning commit):
- `notification_channels.kind`: `slack`, `webhook`, `email`. `config_enc`: `SecretBox` sealed JSON of `ChannelConfig` (K3), AAD `notification_channels:config_enc:<id>`. `target_hint`: K3's `target_hint()`.
- `alert_rules.metric`: `AlertMetric` snake case (a copy of `config.metric`, for listing). `config`: the normalized `AlertCondition` JSON. `state`: `AlertState::as_str()`. `clear_streak`: consecutive clear or insufficient evaluations while firing (D-K-4). `last_value`/`last_count`: the last successful evaluation. `last_notified_at`: the last `fired` or `reminder` event. `last_error`: the last failed evaluation's message, cleared by a successful one.
- `alert_events.kind`: `fired`, `resolved`, `reminder`. `details`: JSON snapshot (K7). Times are ns.
- `notification_deliveries.status`: `pending`, `sent`, `failed`. `next_attempt_at`: set while `pending`, NULL otherwise.

### Version numbers in existing tests

The schema version grows from `P` to `N` (migration numbers are contiguous, so `P` is also the number of migration files before yours). Edit whatever literals are present today; on a tree with two migrations they are `2`/`3`, with tracks H and I merged they are larger:
- `meta/mod.rs`, test `open_twice_is_idempotent`: `assert_eq!(count, P);` → `assert_eq!(count, N);`.
- `meta/mod.rs`, test `open_existing_checks_the_file_and_the_version`: the inserted future migration `VALUES (P+1, 'future', …)` → `VALUES (N+1, 'future', …)`, `assert_eq!(found, P+1)` → `assert_eq!(found, N+1)`, `assert_eq!(expected, P)` → `assert_eq!(expected, N)`.
- `admin.rs`, test `schema_mismatch_names_the_versions`: `VALUES (P+1, 'future', …)` → `VALUES (N+1, 'future', …)` and `"this binary expects schema P, found P+1"` → `"this binary expects schema N, found N+1"`.
- Then run `grep -rn "'future'\|expects schema\|assert_eq!(count, " crates/server/src` and fix any other hard-coded version the same way.

Regenerate the snapshot: `INSTA_UPDATE=always cargo test -p llmobs-server schema_matches_the_snapshot`. `git diff crates/server/tests/snapshots/meta_schema.snap` must show only added blocks for the five tables, their nine explicit indexes and five `sqlite_autoindex_*` primary-key indexes. Anything else: STOP and report.

## 2. `crypto.rs`

Append `("notification_channels", "config_enc"),` to `ENCRYPTED_COLUMNS`. The test `encrypted_columns_cover_the_schema` then passes again (it fails without this row once the migration exists). Key rotation (D-I-3) re-seals the column with the same AAD rule, so nothing else changes.

## 3. `auth/roles.rs`

- Add to `ProjectPerm`, after `Annotate`:

```rust
    /// Alert rules and notification channels (spec 08: member and above; D-K-9).
    ManageAlerts,
```

- `min_role`: `Self::Annotate | Self::ManageAlerts => Role::Member,` (merge with the `Annotate` arm).
- `key_scope`: `Self::ManageSettings | Self::ManageMembers | Self::ManageAlerts => None,`.
- If track I added `ManageEvaluation` with the same rules, keep the arms separate or merged as rustfmt and clippy accept; the behaviour above is what matters.
- In the test `permissions_follow_the_spec_table` add:

```rust
        assert_eq!(ProjectPerm::ManageAlerts.min_role(), Role::Member);
        assert!(ProjectPerm::ManageAlerts.key_scope().is_none());
```

Fix any other exhaustive `match` on `ProjectPerm` the compiler reports (e.g. in `http/access.rs` or `access_suite.rs`) by treating `ManageAlerts` like `Annotate` for roles and like `ManageSettings` for keys.

## 4. `alerts/channel.rs`: sealing

Add:

```rust
use uuid::Uuid;

use crate::crypto::{Aad, CryptoError, SecretBox};

/// AAD table and column of a channel config.
const TABLE: &str = "notification_channels";
const COLUMN: &str = "config_enc";

/// Why a stored channel config could not be read. No variant holds config text.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenConfigError {
    /// The value was sealed with another key, or is corrupt.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// The plaintext is not a `ChannelConfig`.
    #[error("the channel config is not valid JSON")]
    Json,
}

/// Seals `config` for the channel row `id`.
pub(crate) fn seal_config(
    secrets: &SecretBox,
    id: &Uuid,
    config: &ChannelConfig,
) -> Result<Vec<u8>, CryptoError> {
    // Serializing a ChannelConfig cannot fail: it holds only strings, a list and a string map.
    #[allow(clippy::expect_used)]
    let json = serde_json::to_vec(config).expect("channel config serializes");
    secrets.seal(&json, Aad { table: TABLE, column: COLUMN, row_id: id })
}

/// Opens the sealed config of the channel row `id`.
pub(crate) fn open_config(
    secrets: &SecretBox,
    id: &Uuid,
    sealed: &[u8],
) -> Result<ChannelConfig, OpenConfigError> {
    let json = secrets.open(sealed, Aad { table: TABLE, column: COLUMN, row_id: id })?;
    serde_json::from_slice(&json).map_err(|_| OpenConfigError::Json)
}
```

Use `Aad`'s actual field names from `crypto.rs` (`table`, `column`, `row_id` in I's plan); if they differ, use the real ones. Tests (in the existing `mod tests` of `channel.rs`):
- `sealed_config_round_trips`: `SecretBox::new(&[7; 32])`, seal a webhook config for id A, open it for id A → equal.
- `sealed_config_is_bound_to_its_row`: open the same bytes with id B → `Err(OpenConfigError::Crypto(_))`.

## 5. `meta/alerts.rs`

Module doc: `//! Alert rules, notification channels, events and deliveries (spec 04 § Alerts, spec 07).` No `cfg_attr` line (`meta/mod.rs` covers dead code until the routes use it).

### Constants

```rust
/// Most channels per project (D-K-5).
pub(crate) const MAX_CHANNELS: i64 = 20;
/// Most rules per project (D-K-5).
pub(crate) const MAX_RULES: i64 = 100;
/// Most channels on one rule (D-K-5).
pub(crate) const MAX_RULE_CHANNELS: usize = 10;

/// Conflict details (409 bodies, through `MetaError::Conflict`).
pub(crate) const CHANNEL_NAME_TAKEN: &str = "a channel with this name already exists";
pub(crate) const RULE_NAME_TAKEN: &str = "an alert rule with this name already exists";
pub(crate) const TOO_MANY_CHANNELS: &str = "a project has at most 20 channels";
pub(crate) const TOO_MANY_RULES: &str = "a project has at most 100 alert rules";
pub(crate) const UNKNOWN_CHANNEL: &str = "a channel does not exist in this project";
```

### Types (exactly these)

```rust
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
```

`ChannelRow` implements `fmt::Debug` by hand, printing `id`, `kind` and `name` only (`finish_non_exhaustive`), so sealed bytes never reach a log. `ProjectId` is `llmobs_core::ids::ProjectId`; `ChannelKind`, `AlertState`, `AlertCondition` are from `crate::api::alerts`.

### Functions (exactly these signatures)

```rust
pub(crate) async fn insert_channel(conn: &mut SqliteConnection, channel: NewChannel, now: i64) -> Result<ChannelRow, MetaError>;
pub(crate) async fn list_channels(pool: &SqlitePool, project_id: ProjectId) -> Result<Vec<ChannelRow>, MetaError>;
pub(crate) async fn channel_by_id(pool: &SqlitePool, project_id: ProjectId, id: Uuid) -> Result<Option<ChannelRow>, MetaError>;
pub(crate) async fn update_channel(conn: &mut SqliteConnection, project_id: ProjectId, id: Uuid, patch: ChannelPatch, now: i64) -> Result<Option<ChannelRow>, MetaError>;
pub(crate) async fn delete_channel(conn: &mut SqliteConnection, project_id: ProjectId, id: Uuid) -> Result<bool, MetaError>;
pub(crate) async fn last_deliveries(pool: &SqlitePool, project_id: ProjectId) -> Result<HashMap<Uuid, LastDelivery>, MetaError>;

pub(crate) async fn insert_rule(conn: &mut SqliteConnection, rule: NewRule, now: i64) -> Result<RuleRow, MetaError>;
pub(crate) async fn list_rules(pool: &SqlitePool, project_id: ProjectId) -> Result<Vec<RuleRow>, MetaError>;
pub(crate) async fn rule_by_id(pool: &SqlitePool, project_id: ProjectId, id: Uuid) -> Result<Option<RuleRow>, MetaError>;
pub(crate) async fn update_rule(conn: &mut SqliteConnection, project_id: ProjectId, id: Uuid, patch: RulePatch, now: i64) -> Result<Option<RuleRow>, MetaError>;
pub(crate) async fn delete_rule(conn: &mut SqliteConnection, project_id: ProjectId, id: Uuid) -> Result<bool, MetaError>;

/// Enabled alert rules in every project of `org_id` (D178's per-org limit, D-K-14).
pub(crate) async fn count_enabled_org_rules(pool: &SqlitePool, org_id: OrgId) -> Result<i64, MetaError>;
```

`count_enabled_org_rules`: `SELECT COUNT(*) FROM alert_rules r JOIN projects p ON p.id = r.project_id WHERE p.org_id = ? AND r.enabled = 1` (`OrgId` is `llmobs_core::ids::OrgId`, bound as `.to_string()`). Disabled rules do not count, so an org at its limit can keep disabled rules.

Behaviour:

- **`insert_channel`**: `SELECT COUNT(*) FROM notification_channels WHERE project_id = ?`; `>= MAX_CHANNELS` → `MetaError::Conflict(TOO_MANY_CHANNELS)`. Insert all columns with `created_at = updated_at = now`. A unique violation → `unique_or(error, CHANNEL_NAME_TAKEN)`.
- **`list_channels`**: `ORDER BY name, id`.
- **`update_channel`**: load the row on `conn` (`WHERE project_id = ? AND id = ?`); `None` when absent. Merge the patch into the loaded values in Rust, moving the patch's fields out (`if let Some(name) = patch.name`; clippy's `needless_pass_by_value` fires if the patch is only borrowed), then run one static `UPDATE notification_channels SET name = ?, config_enc = ?, target_hint = ?, updated_at = ? WHERE project_id = ? AND id = ?`. Unique violation → `CHANNEL_NAME_TAKEN`. Return the updated row.
- **`delete_channel`**: `DELETE … WHERE project_id = ? AND id = ?`; `true` when a row went. `alert_rule_channels` and `notification_deliveries` rows go by cascade (foreign keys are on for the writer and the readers: `meta/mod.rs`, `.foreign_keys(true)`).
- **`last_deliveries`**: for every channel of the project, its newest delivery with status `sent` or `failed`, by `updated_at` then `id`:

```sql
SELECT d.channel_id, d.status, d.updated_at, d.last_error
FROM notification_deliveries d
JOIN notification_channels c ON c.id = d.channel_id
WHERE c.project_id = ? AND d.status IN ('sent', 'failed')
  AND d.id = (SELECT d2.id FROM notification_deliveries d2
              WHERE d2.channel_id = d.channel_id AND d2.status IN ('sent', 'failed')
              ORDER BY d2.updated_at DESC, d2.id DESC LIMIT 1)
```

- **`insert_rule`**: count rules (`>= MAX_RULES` → `TOO_MANY_RULES`). Sort and dedup `channel_ids` (`sort(); dedup();`). Check each id with the static query `SELECT COUNT(*) FROM notification_channels WHERE project_id = ? AND id = ?` in a loop (at most 10 ids; the route enforces `MAX_RULE_CHANNELS`, the store does not); any count of 0 → `MetaError::Conflict(UNKNOWN_CHANNEL)`. Never build SQL text at run time: sqlx 0.9 accepts only static query strings here. Insert the rule with `id = Uuid::now_v7()`, `metric = condition.metric.as_str()`, `config = serde_json::to_string(&condition)` (cannot fail: comment the invariant on the `expect`, as in § 4), `state = 'ok'`, `clear_streak = 0`, the other state columns NULL, `created_at = updated_at = now`. Unique violation → `RULE_NAME_TAKEN`. Then insert one `alert_rule_channels` row per channel. Return the row (`channel_ids` sorted).
- **`list_rules`**: rules `ORDER BY name, id`; then one query `SELECT rc.rule_id, rc.channel_id FROM alert_rule_channels rc JOIN alert_rules r ON r.id = rc.rule_id WHERE r.project_id = ? ORDER BY rc.rule_id, rc.channel_id` to fill `channel_ids`.
- **`rule_by_id`**: the rule plus its channel ids (`ORDER BY channel_id`).
- **`update_rule`**: load the current row on `conn`; `None` when absent. Merge the patch in Rust (moving its fields out, as above), then run one static `UPDATE alert_rules SET name = ?, metric = ?, config = ?, enabled = ?, cooldown_s = ?, notify_while_firing = ?, state = ?, clear_streak = ?, last_value = ?, last_count = ?, last_evaluated_at = ?, last_error = ?, updated_at = ? WHERE project_id = ? AND id = ?`. The merge rules:
  - `name`, `cooldown_s`, `notify_while_firing`: set when `Some`.
  - `condition`: when `Some(c)` and `c != current.condition`, set `metric` and `config` **and reset the state** (below).
  - `enabled`: when `Some(false)` and the rule is enabled, set `enabled = 0` **and reset the state**. When `Some(true)` and the rule is disabled, set `enabled = 1` and reset the state too (so it is evaluated at the next tick).
  - Reset = `state = 'ok'`, `clear_streak = 0`, `last_value = NULL`, `last_count = NULL`, `last_evaluated_at = NULL`, `last_error = NULL` (D-K-4). `last_fired_at` and `last_notified_at` are kept.
  - `channel_ids`: when `Some`, sort, dedup and check them as in `insert_rule`, then `DELETE FROM alert_rule_channels WHERE rule_id = ?` and insert the new set.
  - `updated_at = now` always (also when nothing else changed). Unique violation on the name → `RULE_NAME_TAKEN`.
  - Return the reloaded row.
- **`delete_rule`**: `DELETE … WHERE project_id = ? AND id = ?`; events, deliveries and links go by cascade.

Imports: `std::collections::HashMap`, `std::fmt`, `llmobs_core::ids::ProjectId`, `sqlx::{SqliteConnection, SqlitePool}`, `uuid::Uuid`, `super::{MetaError, unique_or}`, `crate::api::alerts::{AlertCondition, AlertState, ChannelKind}`.

Records: private `#[derive(Debug, sqlx::FromRow)] struct ChannelRecord` and `struct RuleRecord` with one field per selected column, named as the column: `String` for ids, texts and `kind`/`state`/`config`; `Vec<u8>` for `config_enc`; `i64` for `INTEGER NOT NULL`; `Option<i64>` for nullable integers; `Option<f64>` for `last_value`; `Option<String>` for `last_error` and `created_by`. Read them with `sqlx::query_as::<_, ChannelRecord>("SELECT id, project_id, kind, name, config_enc, target_hint, created_at, updated_at FROM notification_channels WHERE …")` (and the rule columns in table order for `RuleRecord`). Ids are stored as `id.to_string()` (hyphenated lower case), the form `Aad` uses.

Decoding (`row_from` style): `kind` through `ChannelKind::parse`, `state` through `AlertState::parse`, `config` through `serde_json::from_str::<AlertCondition>`, ids through `.parse()` (as `api_keys.rs` does). Any failure → `MetaError::Corrupt { table, column }` with the real table and column names. `enabled` and `notify_while_firing` are `INTEGER` 0/1 (`!= 0`).

### Tests (`#[cfg(test)] mod tests`, with `crate::meta::test_db()`)

Create one org and two projects inside one `db.write`, as `meta/projects.rs`'s tests do: `let org = crate::meta::orgs::insert_org(conn, "O", "o", 1).await?; let a = crate::meta::projects::insert_project(conn, org.id, "A", "a", None, 1).await?; let b = …(conn, org.id, "B", "b", None, 1)…` (check the exact signatures in those files). No user is needed (`created_by: None`). Use `SecretBox`-free byte vectors (`vec![1, 2, 3]`) for `config_enc`: the store does not open them. `MetaError` has no `PartialEq`: assert errors with `matches!`, e.g. `assert!(matches!(error, MetaError::Conflict(CHANNEL_NAME_TAKEN)))` and `assert!(matches!(error, MetaError::Corrupt { table: "alert_rules", column: "state" }))`.

1. `channel_crud`: insert, `channel_by_id`, `list_channels` (ordered by name), `update_channel` (name and config), `delete_channel` → `true`, then `false`; `channel_by_id` and `update_channel` with project B and project A's channel id → `None`; the raw `SELECT id FROM notification_channels` equals `row.id.to_string()`.
2. `channel_names_are_unique_per_project`: a second channel with the same name → `Conflict(CHANNEL_NAME_TAKEN)`; the same name in another project → ok.
3. `channel_limit`: 20 inserts succeed, the 21st → `Conflict(TOO_MANY_CHANNELS)`.
4. `rule_crud_and_channels`: insert a rule with two channels given in reverse order plus a duplicate; `rule_by_id` returns the condition, the two ids sorted, `state Ok`, `clear_streak 0`; `update_rule` with `channel_ids Some(vec![one])` replaces the set; `update_rule` with project B → `None`; `delete_rule` cascades the links (`SELECT COUNT(*) FROM alert_rule_channels` is 0).
5. `rule_rejects_foreign_channels`: a channel of another project → `Conflict(UNKNOWN_CHANNEL)`, on insert and on update.
6. `rule_limit_and_names`: 100 rules ok, the 101st → `TOO_MANY_RULES`; duplicate name → `RULE_NAME_TAKEN`.
7. `condition_change_resets_state`: set the rule's state columns by SQL (`state = 'firing', clear_streak = 1, last_value = 0.9, last_evaluated_at = 5, last_error = 'x', last_fired_at = 4`); `update_rule` with only `name` keeps them; with a different condition → `state Ok`, `clear_streak 0`, `last_value None`, `last_evaluated_at None`, `last_error None`, `last_fired_at Some(4)`; with an equal condition → not reset.
8. `disabling_resets_state`: the same with `enabled Some(false)`; then `enabled Some(true)` keeps `Ok`.
9. `deleting_a_channel_unlinks_it`: a rule with channel C; `delete_channel(C)` → the rule's `channel_ids` is empty.
10. `last_deliveries_picks_the_newest_finished`: insert a rule with `insert_rule`, then an event by SQL: `INSERT INTO alert_events (id, rule_id, kind, threshold, window_from, window_to, details, created_at) VALUES (?, ?, 'fired', 1.0, 0, 1, '{}', 1)`, and deliveries with `INSERT INTO notification_deliveries (id, event_id, channel_id, status, last_error, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)` (`pending` at t=30, `failed` at t=10 with `last_error 'timeout'`, `sent` at t=20) → `LastDelivery { status: "sent", at: 20, error: None }`; a channel with only `pending` deliveries is absent from the map.
12. `enabled_rules_are_counted_per_org`: two projects A and B of one org with 2 enabled rules and 1 disabled rule in all, and one enabled rule in a project of another org → `count_enabled_org_rules(org)` is 2.
11. `corrupt_rows_are_reported`: `UPDATE alert_rules SET state = 'weird'` → `list_rules` is `Err(MetaError::Corrupt { table: "alert_rules", column: "state" })`.

## Commands

```bash
just fmt
```

```bash
cargo test -p llmobs-server meta::alerts
```

```bash
cargo test -p llmobs-server alerts::channel
```

```bash
just check
```

## Done when

- The migration, store, permission and sealing exist exactly as above; the schema snapshot shows only the new tables and indexes.
- `encrypted_columns_cover_the_schema`, `permissions_follow_the_spec_table` and the 14 new tests pass; `just check` is green.
