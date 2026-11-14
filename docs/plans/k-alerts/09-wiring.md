# K9 — Background loops in `llmobs serve`, end-to-end test

**Commit subject:** `server: run the alert loops`

Starts three background tasks with the server: evaluation every 60 s (K7), delivery every 10 s and pruning every hour (K8). Adds an end-to-end test that goes from a rule and a webhook channel created over HTTP, through committed spans, to a signed POST on a receiver.

## Read first

- README § Design → Evaluation and Delivery, D-K-3, D-K-7, D-K-10.
- `crates/server/src/serve.rs`: `bind` (the `Running { … tasks: vec![…] }` literal), `spawn_usage`, `spawn_purge`, `Running::shutdown` (tasks are aborted, then awaited).
- `crates/server/src/live.rs`: `StreamTiming` with `DEFAULT` and `TEST` (the pattern for `AlertTiming`).
- K7 (`evaluate_once`, `EvaluateReport`), K8 (`deliver_once`, `prune_once`, `HookReceiver`).
- `crates/server/src/test_support/mod.rs`: `TestApp`, `with_config`, `seed`, `commit_spans`, `request` (`TestRequest::header`, `::json`).
- `AppInner.eval_runner` (track I's M1, on `main`) is built from `config.security.allow_private_network_egress`, also in `TestApp`.

## Files

| File | Change |
|---|---|
| `crates/server/src/alerts/mod.rs` | `AlertTiming`, `spawn_loops`; remove the `cfg_attr(…dead_code)` line |
| `crates/server/src/serve.rs` | start the loops in `bind` |
| `crates/server/src/alerts/evaluate.rs` | the end-to-end test (in its `mod tests`) |

## 1. `alerts/mod.rs`

Remove the line `#![cfg_attr(not(test), allow(dead_code))] …` added in K3: everything is used now. Every constant of K7 and K8 must be read by code, not only by tests (`CLEARS_TO_RESOLVE` in `step`, `BATCH`, `SEND_TIMEOUT`, `MAX_ATTEMPTS`, `EVENT_RETENTION_NS`, `DELIVERY_RETENTION_NS`, `DELIVERY_HEADER`, `CONCURRENCY`, `QUERY_TIMEOUT`, `DUE_SLACK_NS`); if one is written as a literal somewhere, use the constant. If the compiler then reports an unused item, report it instead of re-adding the allow.

Add:

```rust
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::http::AppState;

/// How often the alert loops run (D-K-3, D-K-7, D-K-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AlertTiming {
    /// Evaluation tick.
    pub(crate) evaluate_every: Duration,
    /// Delivery tick.
    pub(crate) deliver_every: Duration,
    /// Pruning tick.
    pub(crate) prune_every: Duration,
}

impl AlertTiming {
    /// Production timing.
    pub(crate) const DEFAULT: Self = Self {
        evaluate_every: Duration::from_secs(60),
        deliver_every: Duration::from_secs(10),
        prune_every: Duration::from_secs(3_600),
    };

    /// Fast timing for tests that run the loops in real time.
    #[cfg(test)]
    pub(crate) const TEST: Self = Self {
        evaluate_every: Duration::from_millis(100),
        deliver_every: Duration::from_millis(100),
        prune_every: Duration::from_secs(3_600),
    };
}

/// Starts the evaluation, delivery and pruning loops. Each loop waits for its pass to finish
/// before the next tick (`MissedTickBehavior::Skip`), so passes never overlap. The first pass
/// runs one period after the start. Errors are logged and the loop goes on.
pub(crate) fn spawn_loops(state: &AppState, timing: AlertTiming) -> Vec<JoinHandle<()>> {
    vec![
        spawn_evaluation_loop(state.clone(), timing.evaluate_every),
        spawn_delivery_loop(state.clone(), timing.deliver_every),
        spawn_history_prune_loop(state.clone(), timing.prune_every),
    ]
}
```

The three private functions are named `spawn_evaluation_loop`, `spawn_delivery_loop` and `spawn_history_prune_loop` (`serve.rs` has its own unrelated `spawn_prune`). Each follows `serve.rs`'s `spawn_usage` exactly (`tokio::time::interval(every)`, `set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip)`, one `interval.tick().await` before the loop, then `loop { interval.tick().await; … }`):

- `spawn_evaluation_loop`: `match evaluate::evaluate_once(&state, state.clock.now_ns()).await { Ok(report) if report.evaluated > 0 => tracing::debug!(evaluated = report.evaluated, failed = report.failed, events = report.events, stale = report.stale, "alert evaluation pass"), Ok(_) => {}, Err(error) => tracing::error!(error = %error, "alert evaluation pass failed") }`.
- `spawn_delivery_loop`: the same with `deliver::deliver_once` and fields `sent`, `retried`, `failed`, message `"alert delivery pass"` / `"alert delivery pass failed"`.
- `spawn_history_prune_loop`: `deliver::prune_once`; only the error is logged here (`prune_once` logs its own counts), message `"alert prune failed"`.

`MetaError` implements `Display` (it is a `thiserror` enum); its text holds no payload.

## 2. `serve.rs`

In `bind`, build the task list before the `Ok(Running { … })` literal:

```rust
    let mut tasks = vec![
        spawn_prune(state.clone()),
        spawn_purge(state.clone()),
        spawn_last_used(state.clone()),
    ];
    tasks.extend(crate::alerts::spawn_loops(&state, crate::alerts::AlertTiming::DEFAULT));
    tasks.push(spawn_usage(state.clone()));
```

and use `tasks,` in the literal, and write `state: Some(state),` there (the last use moves `state`; a clone would trip clippy's `redundant_clone`). The list must contain every task it contained before. If track I's later milestones added their own tasks or a stop signal to `bind`/`Running`, leave them as they are and only add `spawn_loops(…)` to the same `tasks` vector. `Running::shutdown` already aborts every task: an aborted delivery stays `pending` and is sent again after the restart (at least once, D-K-7). Nothing else changes in `serve.rs`.

## 3. End-to-end test (`alerts::evaluate::tests`)

```rust
#[tokio::test(flavor = "multi_thread")]
async fn rule_to_signed_webhook_end_to_end() { … }
```

Imports: `std::time::Duration`, `axum::http::Method`, `llmobs_eval::{Secret, sign_payload}`, `crate::alerts::{AlertTiming, spawn_loops}`, `crate::test_support::hook_receiver::HookReceiver`. Requests: `app.request(Method::POST, &path).header("cookie", &seeded.cookie).json(&body).send().await` (as `routes/api_keys.rs` tests do).

1. `let app = TestApp::with_config(|config| config.security.allow_private_network_egress = true).await; let seeded = app.seed().await;`
2. `let receiver = HookReceiver::start(vec![200]).await;`
3. POST `/api/v1/projects/{project}/channels` with Ada's cookie: `{"name":"Hook","kind":"webhook","url": receiver.url("/hook"),"secret":"0123456789abcdef"}` → `201`; keep its `id`.
4. POST `/api/v1/projects/{project}/alert-rules`: `{"name":"Errors","condition":{"metric":"error_rate","window":"1m","comparator":"gt","threshold":0.4,"min_count":1},"channel_ids":[<id>]}` → `201`; keep its `id`.
5. Commit two traces with K7's `root_span` helper: `root_span(&app, 1, Duration::from_secs(10), false)` and `root_span(&app, 2, Duration::from_secs(10), true)`.
6. `let tasks = crate::alerts::spawn_loops(&app.state, AlertTiming::TEST);` The manual clock does not move; the rule was never evaluated, so the first evaluation pass fires it, and the delivery pass sends it (its `next_attempt_at` is the same manual `now`).
7. Wait up to 10 s, polling every 50 ms (`tokio::time::sleep`), until `receiver.received().len() == 1`. Then wait 300 ms more and assert it is still exactly 1 (the next evaluations are not due and the rule stays firing without a reminder).
8. The request: path `/hook`; the body parses as JSON with `type "alert.fired"`, `rule.id` = the rule id, `count 2`, `value 0.5`; `Some(header)` equals `sign_payload(&Secret::new("0123456789abcdef".to_owned()), t, &body).as_deref()` where `t: i64` is the number between `t=` and `,` in the header; `x-llmobs-delivery` parses as a UUID equal to the delivery id of step 9.
9. GET `…/alert-rules/{rule id}` → `state "firing"`. GET `…/alert-rules/{rule id}/events` → a page whose `data` has one event with `kind "fired"` and `deliveries` of length 1 with `status "sent"` and `attempts 1`. (The delivery pass may write after the receiver records the request: poll this GET for up to 2 s until the status is `sent`.)
10. `for task in tasks { task.abort(); let _ = task.await; }` (releases the state's clones before the test's temporary directory is dropped).

## 4. Startup and shutdown

No new test: the existing `serve.rs` tests `healthz_over_tcp_and_shutdown_releases_the_lock` and `idle_keepalive_shuts_down_within_the_drain` now start the three loops and check that shutdown still releases the lock in time.

## Commands

```bash
just fmt
```

```bash
cargo test -p llmobs-server alerts::
```

```bash
just check
```

## Done when

- `llmobs serve` runs the three loops and stops them on shutdown.
- The end-to-end test passes reliably (run it 5 times: `for i in 1 2 3 4 5; do cargo test -p llmobs-server rule_to_signed_webhook_end_to_end || break; done`); `just check` is green.
