Status: planned on 2026-11-14; K1–K12 not started

# K · Alerts

Track K of Wave 2 adds alerting (spec 07, D28): per-project rules over four metrics, a background evaluation loop over the query layer, firing/resolved state, and notifications to Slack, a signed webhook and email, with history and the UI pages.

There is no design doc. The pieces exist: A2's query layer (filter DSL, exclusions, `QueryContext`), D's mailer, E's SSRF-guarded egress client and webhook signature, G's server wiring, I's sealed columns, and L's settings layout. The design below fits in this README. The user settled the product choices on 2026-11-14 (§ Decisions).

The plan has 12 sub-phases, **K1–K12**, one file each. Each sub-phase is one commit, done in order by one agent working alone. The implementer is GLM; nothing in a phase file may need a guess. A sub-phase ends with `just check` green.

Alerts are core (MIT), not `ee/` code: the Free plan includes one alert rule (D172, D178, D-K-15).

## Prerequisite

**Track I's M1** (`docs/plans/i-online-eval/01-m1-secrets-connections.md`) is merged on `main`. K uses what it added:
- `crates/server/src/crypto.rs`: `SecretBox` (`seal`, `open`), `Aad { table, column, row_id }`, `CryptoError`, and `ENCRYPTED_COLUMNS` with its test `encrypted_columns_cover_the_schema`;
- `AppInner.secrets: crate::crypto::SecretBox`;
- the `llmobs-eval` dependency in `crates/server/Cargo.toml`, and `config.security.allow_private_network_egress`;
- `crate::test_support::TestApp::with_config`.

- `AppInner.eval_runner: Arc<llmobs_eval::EvalRunner>`, built from that policy (also in `TestApp`).

K4 still checks that `crates/server/src/crypto.rs` has `pub(crate) struct SecretBox` and `pub(crate) const ENCRYPTED_COLUMNS`, and stops otherwise.

## Read first, for every sub-phase

- `AGENTS.md`;
- this README, all of it;
- the sub-phase file;
- `docs/spec/07-alerts.md` (as amended by this plan);
- the existing files the sub-phase names. Read every file before you edit it.

## Files

| File | Sub-phase |
|---|---|
| [01-storage-read.md](01-storage-read.md) | K1 `alert_value` and `check_alert_filter`: one storage read per evaluation |
| [02-egress.md](02-egress.md) | K2 public egress client and payload signature in `llmobs-eval` |
| [03-types.md](03-types.md) | K3 condition and channel config types, with validation |
| [04-store.md](04-store.md) | K4 migration, `meta::alerts` store, `ProjectPerm::ManageAlerts`, sealed channel config |
| [05-channels-api.md](05-channels-api.md) | K5 channel routes, OpenAPI, fixtures |
| [06-rules-api.md](06-rules-api.md) | K6 rule routes, event history, the plan limit check, OpenAPI, fixtures |
| [07-evaluate.md](07-evaluate.md) | K7 the evaluation step and the state machine (manual clock) |
| [08-deliver.md](08-deliver.md) | K8 messages, delivery with retries, test send, pruning |
| [09-wiring.md](09-wiring.md) | K9 background loops in `llmobs serve`, end-to-end test |
| [10-web-channels.md](10-web-channels.md) | K10 API helpers and the Channels settings page |
| [11-web-alerts.md](11-web-alerts.md) | K11 Alerts nav entry, rules list, rule detail with history |
| [12-web-editor.md](12-web-editor.md) | K12 rule editor with the preview chart, dev write mocks, closeout |

## Design

### Model

```
notification_channels ◀──┐ alert_rule_channels (rule_id, channel_id)
        ▲                │
        │           alert_rules ──▶ alert_events ──▶ notification_deliveries ──▶ channel
        │ (sealed config)        (fired/resolved/      (pending → sent | failed,
        │                         reminder)             retries with backoff)
```

- **Channel**: per project (D-K-5). `kind` is `slack`, `webhook` or `email`. Its whole config is one sealed JSON blob (`config_enc`, I's `SecretBox`, AAD `notification_channels:config_enc:<id>`). Responses never contain a URL path, a secret or a header value. They show `target_hint` (D-K-6).
- **Rule**: per project. Only enabled rules of active projects in active orgs are evaluated. A flat `AlertCondition` (`metric`, `window`, `filter`, `comparator`, `threshold`, plus `min_count`, `target`, `score_name`, `aggregation` where they apply; D-K-2), the channel ids, `enabled`, `cooldown_s` and `notify_while_firing`, and its state columns.
- **Event**: one row per notification-worthy transition (`fired`, `resolved`, `reminder`), with a snapshot of the rule (`details`) so history survives later edits.
- **Delivery**: one row per (event, channel). It is created in the same transaction as the event, and is sent later by the delivery loop.

### Evaluation (D-K-3, D-K-4)

```
every 60 s: evaluate_once(state, now)
  rules = enabled rules where last_evaluated_at IS NULL
          or last_evaluated_at + interval(window) − 5 s ≤ now
  for each rule (at most 4 at a time):
     window = [now − window, now)
     storage::alert_value(ctx, AlertValueRequest{measure, window, filter, timeout: 30 s}) → {count, value}
     step(rule state, outcome, now) → new state + optional event   (pure function)
     one meta.write: UPDATE alert_rules … WHERE id = ? AND updated_at = <read value>,
                     INSERT alert_events, INSERT notification_deliveries (one per channel)
```

`interval(window)` is 60 s for windows up to 15 minutes, 300 s otherwise. Only enabled rules of active projects in active orgs are due. The state machine is in § Decisions, D-K-4. The query context comes from `routes::read::open_query_on`, the helper every read uses, so the plan's data-access window that track P will apply there (D175) reaches alerts with no K code (D-K-16).

### Delivery (D-K-7, D-K-8)

```
every 10 s: deliver_once(state, now)
  rows = deliveries where status = 'pending' and next_attempt_at ≤ now, oldest first, at most 50
  for each (at most 4 at a time): open the channel config, build the message, send
     2xx / mail sent           → status 'sent'
     retryable failure         → attempts += 1, next_attempt_at = now + backoff(attempts), or 'failed' after 8
     permanent failure         → 'failed'
every hour: prune_once(state, now): events older than 90 days, finished deliveries older than 30 days
```

Slack and the webhook go through `llmobs_eval::EgressClient` (K2, lent by `state.eval_runner.egress()`), the SSRF-guarded client. Email goes through `crate::email::Mailer` with a new `alert` template. Delivery is at least once: a crash between the send and the status write sends again. The webhook carries `X-Llmobs-Delivery: <delivery id>` so a receiver can drop duplicates.

### Modules (end state)

```
crates/storage/src/reads/alerts.rs     AlertMeasure, AlertValueRequest, AlertValue, alert_value, check_alert_filter (K1)
crates/eval/src/egress.rs              EgressClient, EgressRequest, EgressResponse made public (K2)
crates/eval/src/webhook.rs             sign_payload, SIGNATURE_HEADER made public (K2)
crates/server/src/api/alerts.rs        enums, AlertCondition (K3); channel types (K5); rule and event types (K6); ChannelTestResult (K8)
crates/server/src/alerts/condition.rs  normalize_condition, measure (K3)
crates/server/src/alerts/channel.rs    ChannelConfig: validate, target_hint (K3); seal_config, open_config (K4)
crates/server/migrations/meta/NNNN_alerts.sql                                              (K4)
crates/server/src/meta/alerts.rs       store for the 5 tables (K4; events read K6; evaluation K7; deliveries K8)
crates/server/src/routes/channels.rs   channel CRUD (K5), test send (K8)
crates/server/src/routes/alert_rules.rs  rule CRUD and events (K6)
crates/server/src/alerts/quota.rs      rule_limit, check_rule_quota (K6, D-K-14)
crates/server/src/alerts/evaluate.rs   step (pure), evaluate_once (K7)
crates/server/src/alerts/message.rs    Slack, webhook and email bodies, links (K8)
crates/server/src/alerts/deliver.rs    deliver_once, send_one, prune_once (K8)
crates/server/src/alerts/mod.rs        AlertTiming, spawn_loops (K9)
crates/server/src/email/templates/alert.{txt,html}                                        (K8)
web/src/routes/$org/$project/settings/channels.tsx                                       (K10)
web/src/routes/$org/$project/alerts/{index,$ruleId/index}.tsx                             (K11)
web/src/routes/$org/$project/alerts/{new,$ruleId/edit}.tsx                                (K12)
web/src/components/alerts/*                                                             (K10–K12)
```

### Migration number

Tracks H and I add migrations in parallel (`main` has `0004_connections.sql` today). K's migration takes **the next free number when K4 is implemented**: the highest `NNNN` in `crates/server/migrations/meta/` plus one, zero-padded to four digits (`0005_alerts.sql` if `0004_connections.sql` is still the last). K4 explains the schema-version tests that change with it.

## Decisions

The user settled these on 2026-11-14. They are written in `docs/mvp-decisions.md` § Track K and in spec 07, in the same commit as this plan.

| # | Decision |
|---|---|
| D-K-1 | **A plan, no design doc.** The building blocks exist (A2, D, E, G, I M1, L); this README holds the short design. |
| D-K-2 | **Four conditions, no `group_by`.** `error_rate`, `cost`, `latency_p95` (`target` `trace` or `llm_span`) and `score` (`aggregation` `avg` or `pass_rate`), each with an optional filter, a comparator (`gt`, `gte`, `lt`, `lte`) and a `threshold`. One state per rule; a per-model cost alert is a rule with a filter. Latency thresholds and values are milliseconds; costs are USD; rates are fractions in `[0, 1]`. The filter is always a `traces`-view expression: span and score measures keep rows whose trace passes it, as on the dashboard (spec 05). `min_count` (1–1 000 000) applies to `error_rate`, `latency_p95` and `score` (defaults 20, 20, 10); `cost` has none. Spec 07's `group_by` and its 20-group cap are dropped. (User's choice.) |
| D-K-3 | **Spec 07's cadence.** A global tick every 60 s. A rule is due when it was never evaluated, or when `last_evaluated_at + interval − 5 s ≤ now`; `interval` is 60 s for windows ≤ 15 m and 300 s otherwise. The window is sliding, `[now − window, now)` on `start_time` (on `created_at` for scores), memtable included. Windows: `1m`, `5m`, `15m`, `1h`, `6h`, `24h`. At most 4 evaluations run at once, each with a 30 s timeout; a tick waits for the previous one. The concurrency is a constant, not spec 07's `alerts.concurrency` config key. Competitors do the same (Helicone: 1 min; Opik: 5 min; `docs/research/alerting.md`). (User's choice.) |
| D-K-4 | **Spec 07's state model.** States `ok`, `firing`, `no_data`. An evaluation is *insufficient* when `count < min_count`, *breaching* when it is not insufficient, its value is present and `value <comparator> threshold`, else *clear*. From `ok` or `no_data`: breaching → `firing` with a `fired` event; clear → `ok`; insufficient → `no_data`; no event for either. From `firing`: breaching resets the clear streak, and sends a `reminder` event when `notify_while_firing` is on and `now − last_notified_at ≥ cooldown_s`; clear or insufficient adds one to the clear streak, and the second in a row resolves: state `ok` (or `no_data` when the last one was insufficient) with a `resolved` event. A failed query changes no state and records `last_error`; it is never treated as no data. `cooldown_s` is 300–604 800, default 3 600; `notify_while_firing` defaults to false. There is no acknowledge or snooze. Changing a rule's `condition`, or disabling or re-enabling it, resets its state to `ok` with no event; renaming it or changing channels, cooldown or `notify_while_firing` keeps the state. (User's choice.) |
| D-K-5 | **Channels are per project**, as spec 04. A rule may use only its own project's channels; deleting a channel removes it from every rule. Limits: 20 channels and 100 rules per project, 10 channels per rule. Names are 1–100 characters, unique per project among channels and among rules. (User's choice.) |
| D-K-6 | **Channel configs are sealed and write-only.** `slack`: `{url}`, an `https` URL. `webhook`: `{url, secret, headers}`, an `http` or `https` URL, a signing secret of 16–256 characters (required, unlike spec 07's optional one), at most 10 extra headers. `email`: `{recipients}`, 1–20 addresses. The whole JSON is sealed in `config_enc`. Responses show `target_hint` and never the URL path, the secret or header values: Slack `hooks.slack.com/…/abcd` (host, then `/…/` and the URL's last 4 characters), webhook the host with its port when present, email the first address plus ` +N` for the others. Email recipients are returned in full to project members (`recipients`); webhook responses list `header_names`. A PATCH may change the name and the config fields; `kind` is immutable; an absent field keeps its value. |
| D-K-7 | **Delivery.** Each (event, channel) is a `notification_deliveries` row, sent by a loop every 10 s, at most 4 at once, 10 s timeout per send. Retryable failures (timeouts, connection errors, HTTP 408, 429 and 5xx, SMTP connection, TLS and authentication failures) wait `min(30 s · 2^(attempts−1), 1 h)` and give up after 8 attempts. Permanent failures (address denied by the egress guard, other 4xx, a bad address, email without SMTP, a config that no longer opens) fail at once. A channel is never disabled automatically; the UI shows its last finished delivery (status, time, error), and "Send test" sends one message synchronously and shows the result. An email delivery sends one message per recipient; a retry sends to every recipient again. (User's choice.) |
| D-K-8 | **Messages.** Slack gets a Block Kit message; the webhook gets spec 07's JSON (amended: `event_id`, `count`, `window_from`, `window_to`, `rule_url`, no `group`), signed like the webhook evaluator (`X-Llmobs-Signature: t=<unix>,v1=<hex>`), plus `X-Llmobs-Delivery`; email uses the new `alert` template. All carry two links from `public_url`: the rule page `/<org>/<project>/alerts/<rule id>` and the trace list over the event's window with the rule's filter. |
| D-K-9 | **Permissions.** A new `ProjectPerm::ManageAlerts`: role `member`, API keys cannot use it, not session-only (spec 08). Reads (`GET` rules, events, channels) use `ProjectPerm::View`. "Send test" needs `ManageAlerts`. (User's choice.) |
| D-K-10 | **History retention.** An hourly prune deletes events older than 90 days (their deliveries go with them) and `sent`/`failed` deliveries older than 30 days. This replaces spec 03's "alert events are kept". (User's choice.) |
| D-K-11 | **Two additions outside `crates/server`.** `llmobs-storage` gets `alert_value`, one aggregate query per evaluation, because the dashboard `metrics` read runs nine collects and has no sample count. `llmobs-eval` makes its egress client public (`EgressClient`, lent by `EvalRunner::egress`) and exposes `sign_payload`, so alert deliveries share the SSRF guard and the signature format. (User's choice.) |
| D-K-12 | **No online-judge pause alerts.** Spec 07's built-in pseudo-alert for paused judge rules is dropped from K; track I shows pauses as banners (D-I-14). |
| D-K-13 | **The rule editor previews over buckets as wide as the window.** It calls `GET /metrics` with `bucket` equal to the rule's window (`24h` uses `1d`), over 24 h for windows up to 1 h, 3 days for `6h`, and 14 days for `24h`, and draws the threshold as a horizontal line. `latency_p95` with `target: llm_span` has no preview (the dashboard splits it by model). |
| D-K-14 | **One plan-limit check for alert rules (D178).** Creating an enabled rule, and re-enabling a disabled one, call `alerts::quota::check_rule_quota(count_enabled_org_rules(org), alerts::quota::rule_limit(state, org))`. Over the limit is `403` with the new problem code `plan_limit` and a detail naming the limit ("Your plan allows 1 enabled alert rule."); the UI shows it with an upgrade line. Only enabled rules of active projects count: on purpose, so an org at its limit can keep disabled rules (D178 says "1 alert rule"; K reads it as one enabled rule). Existing rules are never disabled by K. `rule_limit` returns `None` (no limit) (`Option<u64>`) until track P's M4 replaces its body with `entitlements::of(state, org_id).limits.max_alert_rules` (D179); it is the only code P changes for alerts. The count is read before the write, so two concurrent creates can end one rule over; accepted. |
| D-K-15 | **Alerts are core (MIT).** The Free plan includes one alert rule, so alerting is not `ee/` code (D172, D178). |
| D-K-16 | **The plan's data window applies through the query layer.** Evaluations build their query context with `routes::read::open_query_on`, like every other read, so the data-access window that track P enforces in A2's time window (D175) hides old data from alerts too; K adds nothing for it. |

## Contract gaps

- **CG-K1.** `GET /metrics` has no `count` per bucket for the score series, so the preview cannot show where `min_count` would give `no_data`. Accepted.
- **CG-K2.** Project slugs can still be `alerts` (L's CG2 is the same issue for `settings`). Static routes win, so a project slug `alerts` is unaffected at `/$org/$project/alerts`; noted only.
- **CG-K3.** There is no project-wide event list (`GET /alert-events`); the history is per rule. A later track can add it.

## Conventions

- **Time**: `i64` nanoseconds in Rust and `meta.db`; RFC 3339 strings with 9 fractional digits in JSON (`llmobs_core::time_serde`, as the existing API types).
- **IDs**: rules, channels, events and deliveries are UUIDv7 (`Uuid::now_v7()`), stored as `TEXT` like every other `meta.db` id.
- **Errors**: problem+json through `crate::http::error::ApiError`. Field errors name the JSON field path, e.g. `condition.threshold`.
- **Privacy**: logs carry rule, channel, event and delivery ids, the channel kind, attempt numbers and error classes. Never a URL, a header value, a secret, an email address, a rule name or a filter (AGENTS.md § Privacy).
- **No `unwrap`/`expect`** outside tests unless a comment states the invariant.
- **Tests**: Rust tests sit in the module they test (`#[cfg(test)] mod tests`). Web tests use MSW with `server.use` overrides and never `vi.mock`.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
just check
```

`just check` must pass before every commit. When a sub-phase changes the API contract, run `just openapi` first and commit `crates/server/openapi.json` and `web/src/api/schema.gen.ts`. Focused runs while working: `cargo test -p llmobs-storage alert`, `cargo test -p llmobs-eval egress`, `cargo test -p llmobs-server alerts`, `pnpm --dir web test -- alerts`.

## Report

When a sub-phase is done, report what was done, any deviation and why, versions of any new dependency (K adds none), and anything left undone or unverified.
