# K7 — The evaluation step and the state machine

**Commit subject:** `server: evaluate alert rules`

Adds `alerts::evaluate`: the pure state machine `step` (D-K-4) and `evaluate_once(state, now)`, which evaluates every due rule once (D-K-3), records the outcome, and creates the events and their pending deliveries. No background task yet (K9 spawns it), no sending yet (K8). Everything is tested with the manual clock.

## Read first

- README § Design → Evaluation, D-K-2, D-K-3, D-K-4.
- K1 (`alert_value`, `AlertValueRequest`, `AlertValue`), K3 (`AlertCondition`, `Comparator::breaches`, `AlertWindow::ns`, `interval_ns`, `alerts::condition::measure`), K4 (`meta::alerts`, `RuleRow`), K6 (`EventDetails`, `EventRow`, `DeliveryRow`, `list_events`, `deliveries_for_events`).
- `crates/server/src/routes/read.rs` (`open_query_on`), `crates/server/src/clock.rs` (`Clock::Manual`, `advance`, `set`).
- `crates/server/src/test_support/mod.rs`: `TestApp`, `seed`, `commit_spans`, `dataset_span`, `assert_logs_clean`, `capture_logs`. The manual clock starts at 2026-01-01T00:00:00Z (`MANUAL_NOW_NS` is private; use `app.state.clock.now_ns()`).
- `crates/storage/src/lib.rs`: `Storage::query`, `QueryOptions` (derives `Default`), `QueryContext::new`, `RunError`; `QueryError` is `llmobs_storage::error::QueryError` (not re-exported at the root).

## Files

| File | Change |
|---|---|
| `crates/server/src/alerts/evaluate.rs` | new |
| `crates/server/src/alerts/mod.rs` | `pub(crate) mod evaluate;` |
| `crates/server/src/meta/alerts.rs` | `due_rules`, `Evaluation`, `Recorded`, `record_evaluation` |

## 1. Store additions (`meta/alerts.rs`)

```rust
/// Enabled rules of active projects in active orgs whose next evaluation is due at `now`:
/// never evaluated, or `last_evaluated_at + interval(window) − slack_ns ≤ now`.
/// Ordered by `last_evaluated_at` (NULL first), then `id`.
pub(crate) async fn due_rules(pool: &SqlitePool, now: i64, slack_ns: i64) -> Result<Vec<RuleRow>, MetaError>;
```

SQL: select every enabled rule of an active project and org, then filter in Rust (the interval depends on the window inside the JSON config):

```sql
SELECT r.<the rule columns, as K4's rule query selects them>
FROM alert_rules r
JOIN projects p ON p.id = r.project_id
JOIN orgs o ON o.id = p.org_id
WHERE r.enabled = 1 AND p.state = 'active' AND o.state = 'active'
```

(`projects.state` and `orgs.state` are `'active'` or `'deleting'`, migration `0001_identity.sql`; the joins only filter.) Decode with K4's `RuleRecord` and its decoder, and fill `channel_ids` with the same per-rule query as `rule_by_id`. Keep a rule when `last_evaluated_at.is_none_or(|at| at.saturating_add(rule.condition.window.interval_ns()).saturating_sub(slack_ns) <= now)`. Sort as documented.

```rust
/// What one evaluation recorded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Evaluation {
    /// The query failed. Only `last_evaluated_at` and `last_error` change.
    Failed { message: String },
    /// The query answered.
    Measured {
        step: Step,
        count: i64,
        value: Option<f64>,
        /// The condition's threshold, copied into the event.
        threshold: f64,
        window_from: i64,
        window_to: i64,
        details: EventDetails,
    },
}

/// Writes one evaluation of `rule_id`, but only if the rule's `updated_at` is still
/// `expected_updated_at`; a rule edited meanwhile is left alone. See [`Recorded`].
pub(crate) async fn record_evaluation(
    conn: &mut SqliteConnection,
    rule_id: Uuid,
    expected_updated_at: i64,
    evaluation: Evaluation,
    now: i64,
) -> Result<Recorded, MetaError>;

/// What [`record_evaluation`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recorded {
    /// The rule changed since it was read; nothing was written.
    Stale,
    /// The evaluation was written; no event.
    Written,
    /// The evaluation was written with this event and `deliveries` pending deliveries.
    Event { event_id: Uuid, deliveries: usize },
}
```

`Step` is `crate::alerts::evaluate::Step` (§ 2); import it.

`record_evaluation` behaviour (it runs inside the caller's `MetaDb::write` transaction):
- `Failed { message }`: `UPDATE alert_rules SET last_evaluated_at = ?, last_error = ? WHERE id = ? AND updated_at = ?`. 0 rows → `Stale`, else `Written`.
- `Measured`: `UPDATE alert_rules SET state = ?, clear_streak = ?, last_value = ?, last_count = ?, last_evaluated_at = ?, last_error = NULL, last_fired_at = CASE WHEN ? THEN ? ELSE last_fired_at END, last_notified_at = CASE WHEN ? THEN ? ELSE last_notified_at END WHERE id = ? AND updated_at = ?` with binds `step.state.as_str()`, `step.clear_streak`, `value`, `count`, `now`, `step.fired`, `now`, `step.notified`, `now`, `rule_id`, `expected_updated_at`. 0 rows → `Stale`. `updated_at` is **not** changed by an evaluation (it marks user edits only).
  - When `step.event` is `Some(kind)`: insert `alert_events (id = Uuid::now_v7(), rule_id, kind.as_str(), value, count, threshold, window_from, window_to, details = serde_json::to_string(&details), created_at = now)`. Then select the rule's channels (`SELECT channel_id FROM alert_rule_channels WHERE rule_id = ? ORDER BY channel_id`) and insert one `notification_deliveries` row per channel: `id = Uuid::now_v7()`, `status = 'pending'`, `attempts = 0`, `next_attempt_at = now`, `last_error = NULL`, `created_at = updated_at = now`. Return `Event { event_id, deliveries }`.
  - Otherwise `Written`.
- `serde_json::to_string(&details)` cannot fail (strings and enums only): put `// EventDetails holds strings and unit enums only; serializing it cannot fail.` and `#[allow(clippy::expect_used)]` on the lines before the `let … = serde_json::to_string(&details).expect("event details serialize");`, as K4 § 4 does.
- Ids are bound as `.to_string()`. The channel ids read from `alert_rule_channels` are parsed with `.parse()`; a failure → `MetaError::Corrupt { table: "alert_rule_channels", column: "channel_id" }`.

## 2. `alerts/evaluate.rs`

Module doc: `//! One evaluation pass over the due rules, and the rule state machine (spec 07, D-K-3, D-K-4).`

### Constants

```rust
/// Rules evaluated at once (D-K-3).
pub(crate) const CONCURRENCY: usize = 4;
/// Time allowed for one rule's query (D-K-3).
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// A rule is due this much early, so a 60 s tick never skips a 60 s interval (D-K-3).
pub(crate) const DUE_SLACK_NS: i64 = 5_000_000_000;
/// Clear or insufficient evaluations in a row that resolve a firing rule (D-K-4).
pub(crate) const CLEARS_TO_RESOLVE: i64 = 2;
```

### The state machine

```rust
/// The state after one successful evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) state: AlertState,
    pub(crate) clear_streak: i64,
    /// The event to record, if any.
    pub(crate) event: Option<AlertEventKind>,
    /// This evaluation fired the rule: `last_fired_at = now`.
    pub(crate) fired: bool,
    /// A `fired` or `reminder` event: `last_notified_at = now`.
    pub(crate) notified: bool,
}

/// How one measurement reads against the condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Fewer samples than `min_count`.
    Insufficient,
    /// Enough samples, a value, and the comparator holds.
    Breaching,
    /// Anything else.
    Clear,
}

pub(crate) fn reading(condition: &AlertCondition, count: i64, value: Option<f64>) -> Reading;

pub(crate) fn step(rule: &RuleRow, reading: Reading, now: i64) -> Step;
```

`reading`: `Insufficient` when `condition.min_count.is_some_and(|min| count < min)`; else `Breaching` when `value.is_some_and(|value| condition.comparator.breaches(value, condition.threshold))`; else `Clear`.

`step`, exactly this table (`streak` is `rule.clear_streak`):

| `rule.state` | reading | new state | clear_streak | event | fired | notified |
|---|---|---|---|---|---|---|
| `ok` or `no_data` | Breaching | `firing` | 0 | `fired` | true | true |
| `ok` or `no_data` | Clear | `ok` | 0 | none | false | false |
| `ok` or `no_data` | Insufficient | `no_data` | 0 | none | false | false |
| `firing` | Breaching, reminder due | `firing` | 0 | `reminder` | false | true |
| `firing` | Breaching, otherwise | `firing` | 0 | none | false | false |
| `firing` | Clear, `streak + 1 < 2` | `firing` | `streak + 1` | none | false | false |
| `firing` | Insufficient, `streak + 1 < 2` | `firing` | `streak + 1` | none | false | false |
| `firing` | Clear, `streak + 1 >= 2` | `ok` | 0 | `resolved` | false | false |
| `firing` | Insufficient, `streak + 1 >= 2` | `no_data` | 0 | `resolved` | false | false |

"Reminder due" is `rule.notify_while_firing && rule.last_notified_at.is_none_or(|at| now.saturating_sub(at) >= rule.cooldown_s.saturating_mul(1_000_000_000))`.

### `evaluate_once`

```rust
/// What one pass did. Logged by the loop (K9) and asserted by tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EvaluateReport {
    pub(crate) evaluated: usize,
    pub(crate) failed: usize,
    pub(crate) events: usize,
    pub(crate) stale: usize,
}

/// Evaluates every due rule once at `now`, at most [`CONCURRENCY`] at a time.
pub(crate) async fn evaluate_once(state: &AppState, now: i64) -> Result<EvaluateReport, MetaError>;
```

Steps:
1. `let due = due_rules(state.meta.reader(), now, DUE_SLACK_NS).await?;`
2. `futures::stream::iter(due).map(|rule| Box::pin(evaluate_rule(state, rule, now))).buffer_unordered(CONCURRENCY).collect::<Vec<_>>().await` (`use futures::StreamExt;`; `futures` is already a server dependency — check `Cargo.toml`; if it is not, STOP and report).
3. Sum the per-rule results into the report. A `MetaError` from one rule's write is logged (`tracing::error!(rule_id = %id, error = %error, "alert evaluation write failed")`) and counted in `failed`; the pass continues. `evaluate_once` returns `Err` only when step 1 fails.

`async fn evaluate_rule(state: &AppState, rule: RuleRow, now: i64) -> (Uuid, bool, Result<Recorded, MetaError>)` returns the rule id, whether the query failed, and the write's result. The report counts: `evaluated` += 1 for every rule; `failed` += 1 when the query failed or the write returned `Err`; `events` += 1 for `Recorded::Event`; `stale` += 1 for `Recorded::Stale`.

Imports: `futures::StreamExt`, `llmobs_core::ids::ProjectId`, `llmobs_storage::{AlertValue, AlertValueRequest, QueryContext, QueryOptions, RunError, TimeWindow, alert_value}`, `llmobs_storage::error::QueryError`, `crate::alerts::condition::measure`, `crate::api::alerts::{AlertCondition, AlertEventKind, AlertState}`, `crate::http::AppState`, `crate::meta::MetaError`, `crate::meta::alerts::{EventDetails, Evaluation, Recorded, RuleRow, due_rules, record_evaluation}`, `std::time::Duration`, `uuid::Uuid`. Remove any the compiler reports unused; run `just fmt`.

`rule` is taken by value and moved apart (clippy's `needless_pass_by_value`): copy `let rule_id = rule.id; let project = rule.project_id; let expected_updated_at = rule.updated_at;` first, and move `rule.name` into `EventDetails` without cloning.

1. `let window_from = now.saturating_sub(rule.condition.window.ns());`
2. Measure:

```rust
async fn measure_rule(state: &AppState, project: ProjectId, condition: &AlertCondition, from: i64, to: i64) -> Result<AlertValue, &'static str>
```

   - `let window = TimeWindow::new(from, to).map_err(|_| "invalid window")?;`
   - `let source = state.storage.query(project, QueryOptions::default()).await` — `Err(QueryError::Busy)` → `"storage is busy"`; any other `Err` → `"query failed"`.
   - `let ctx = QueryContext::new(source, None).map_err(|_| "query failed")?;`
   - `Box::pin(alert_value(&ctx, AlertValueRequest { window, filter: condition.filter.as_deref(), measure: measure(condition), timeout: QUERY_TIMEOUT })).await` (boxed: DataFusion futures are large) with errors mapped: `RunError::Timeout` → `"query timed out"`, `RunError::TooLarge` → `"query needed more memory than allowed"`, `RunError::Invalid(_)` → `"the filter is no longer valid"`, every other variant → `"query failed"`.
   - On error, the caller logs `tracing::warn!(rule_id = %rule_id, reason, "alert evaluation failed")` (the reason is one of the fixed strings above, never a filter or a query message).
3. Build the evaluation:
   - error → `Evaluation::Failed { message: reason.to_owned() }`;
   - ok → `let read = reading(&rule.condition, measured.count, measured.value); let next = step(&rule, read, now);` and `Evaluation::Measured { step: next, count: measured.count, value: measured.value, threshold: rule.condition.threshold, window_from, window_to: now, details: EventDetails { rule_name: rule.name, metric: rule.condition.metric, comparator: rule.condition.comparator, window: rule.condition.window, filter: rule.condition.filter } }` (compute `read` and `next` before moving the fields out of `rule`; keep `let event_kind = next.event;`, `Step` is `Copy`).
4. `state.meta.write(move |conn| Box::pin(async move { record_evaluation(conn, rule_id, expected_updated_at, evaluation, now).await })).await`.
5. When the result is `Recorded::Event { event_id, deliveries }`, log `tracing::info!(rule_id = %rule_id, event_id = %event_id, kind = event_kind.map_or("", AlertEventKind::as_str), deliveries, "alert event")`.

`state.storage` is `Arc<Storage>`; the project is `rule.project_id`.

## 3. Tests

### Pure (`alerts::evaluate::tests`)

Build a `RuleRow` with a helper `fn rule(state: AlertState, streak: i64) -> RuleRow` (`id: Uuid::now_v7()`, `project_id: ProjectId::new_v7()` — use the constructor `llmobs_core::ids` has —, an `error_rate` condition, threshold 0.1, `min_count Some(10)`, `notify_while_firing false`, `cooldown_s 3600`, `last_notified_at None`, other optional fields `None`, `enabled true`, times 0).

1. `reading_classifies`: count 9 → `Insufficient` (whatever the value); count 10, value 0.2 → `Breaching`; value 0.1 → `Clear` (`gt`); value `None` → `Clear`; a `cost` condition (`min_count None`) with count 0 and value 0.0, threshold 0.0, `gte` → `Breaching`.
2. `step_table`: one assertion per row of the table above (nine rows), comparing the whole `Step`. For the row "Breaching, reminder due" set `notify_while_firing = true` and `last_notified_at = None`; every other row uses the helper as is.
3. `reminders_respect_the_cooldown`: firing, `notify_while_firing true`, `last_notified_at Some(now − 3599 s)` → no event; `now − 3600 s` → `reminder`, `notified true`; `notify_while_firing false` → never.

### Store (`meta::alerts::tests`)

4. `due_rules_respects_interval_and_state`: rules (inserted with `insert_rule`, no channels) with windows `5m` and `1h`, both marked evaluated at `t` by `UPDATE alert_rules SET last_evaluated_at = ?`: at `t + 54 s` none is due; at `t + 55 s` (slack 5 s) the `5m` rule is due; at `t + 295 s` both; a disabled rule, and a never-evaluated rule of a second project set to `state = 'deleting'` by SQL, are never due; a never-evaluated enabled rule is due at once.
5. `record_evaluation_is_fenced`: `Measured` with `expected_updated_at = rule.updated_at + 1` → `Stale` and no change; with the right one and `event Some(Fired)` and two channels → `Event { deliveries: 2 }`, the rule row shows `state firing`, `last_fired_at`, `last_notified_at`, `last_error NULL`, and two `pending` deliveries with `next_attempt_at = now`; `Failed` sets `last_error` and `last_evaluated_at` and keeps the state.

### Through storage, with the manual clock (`alerts::evaluate::tests`)

Use `TestApp::new().await` and `app.seed().await`. The app's clock is `Clock::Manual` at `MANUAL_NOW_NS`. Build traces with a helper:

```rust
/// One root span of a new trace, starting `ago` before now, lasting 1 ms.
fn root_span(app: &TestApp, index: u8, ago: Duration, error: bool) -> Span {
    let mut span = crate::test_support::dataset_span(0);
    let start = app.state.clock.now_ns() - i64::try_from(ago.as_nanos()).expect("small");
    span.trace_id = TraceId::from_bytes([index; 16]).expect("trace id");
    span.span_id = SpanId::from_bytes([index; 8]).expect("span id");
    span.parent_span_id = None;
    span.start_time = start;
    span.end_time = start + 1_000_000;
    span.status = if error { SpanStatus::Error } else { SpanStatus::Ok };
    span.environment = None;
    span.metadata.clear();
    span
}
```

(Check `TraceId::from_bytes`/`SpanId::from_bytes` in `llmobs_core::ids`; use the constructor that exists. `index` starts at 1: an all-zero id is invalid in OTel.) Commit them with `app.commit_spans(&seeded.project, spans).await`. Insert the rule and a Slack channel through `meta::alerts` (seal with `alerts::channel::seal_config(&app.state.secrets, …)`).

6. `fires_then_resolves_with_the_manual_clock`: rule `error_rate`, `5m`, `gt 0.4`, `min_count 2`, one channel. Commit 3 traces 1 minute ago, 2 with errors.
   Use rule `cooldown_s 3600`, `notify_while_firing false`. Read events with `list_events(app.state.meta.reader(), rule.id, None, 10)` (newest first) and deliveries with `deliveries_for_events`.
   - `evaluate_once(&app.state, now)` → `EvaluateReport { evaluated: 1, events: 1, ..Default::default() }`; the rule is `firing`, `last_value Some(2/3)`, `last_count Some(3)`; one event `fired` (`list_events`) with `value 2/3`, `count 3`, `threshold 0.4`, `window_to = now`, `window_from = now − 5 min`; one `pending` delivery.
   - Advance 30 s: `evaluate_once` → `evaluated 0` (not due).
   - Advance 30 s more (60 s after the first): due; still breaching → no new event.
   - Advance 6 minutes (the traces leave the window): count 0 < 2 → insufficient, streak 1, still `firing`, no event.
   - Advance 60 s: streak 2 → `no_data` with a `resolved` event whose `value` is absent and `count` is 0; one more pending delivery.
7. `a_failed_query_records_the_error`: a rule inserted with `insert_rule` and a condition whose `filter` is `Some("nonsense_column = 1".to_owned())` (the store does not check filters) → `evaluate_once` reports `failed 1`; the rule keeps `state ok`, `last_error "the filter is no longer valid"`, `last_evaluated_at = now`; no event.
8. `an_edit_during_evaluation_is_not_overwritten`: read the rule, then `update_rule(…, now + 1)` (a later time, so `updated_at` changes), then `record_evaluation` with the old `updated_at` → `Stale`; the rule keeps the edit.
9. `evaluation_logs_hold_no_filter`: a rule with filter `"missing_column = 'secret-env'"` (it fails to parse) → `crate::test_support::assert_logs_clean(&["secret-env", "missing_column"], …)` around `evaluate_once`; afterwards the rule's `last_error` is `"the filter is no longer valid"`.

## Commands

```bash
just fmt
```

```bash
cargo test -p llmobs-server alerts::evaluate
```

```bash
cargo test -p llmobs-server meta::alerts
```

```bash
just check
```

## Done when

- `step` follows the table exactly; `evaluate_once` evaluates due rules, records failures without state changes, and creates events with pending deliveries in one transaction, fenced by `updated_at`.
- The 9 tests pass; `just check` is green.
