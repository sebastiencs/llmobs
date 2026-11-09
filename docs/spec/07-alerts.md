# 07 — Alerts

## Rule model

`alert_rules` (04). `config` per metric:

| `metric` | Config | Computed over the window |
|---|---|---|
| `error_rate` | `{filter?, window: "5m", min_count: 20, comparator: ">", threshold: 0.05}` | `sum(error_count > 0) / count(*)` on `traces` |
| `cost` | `{filter?, window: "1h", comparator: ">", threshold: 50.0, group_by?: "model"\|"user_id"\|"environment"}` | `sum(cost_total)` on `traces` (or `spans` when grouped by model) |
| `latency_p95` | `{filter?, window: "15m", target: "trace"\|"llm_span", min_count: 20, comparator: ">", threshold_ms: 8000}` | `approx_percentile_cont(duration_ns, 0.95)` |
| `score` | `{score_name, filter?, window: "1h", aggregation: "avg"\|"pass_rate", min_count: 10, comparator: "<", threshold: 0.7}` | `avg(value_num)` on `scores` (pass rate = avg of boolean) |

Evaluation traces (`environment = 'llmobs-eval'`) and experiment traces are excluded from every alert metric (05 §Default exclusions); judge spend is covered by rule budgets (06).

Common fields: `window` ∈ {1m, 5m, 15m, 1h, 6h, 24h}; `filter` is a filter-DSL expression on the metric's view; `min_count` avoids firing on tiny samples (`no_data` state instead); `cooldown` (default 1 h) — minimum time between two notifications of the same rule while firing; `group_by` (cost only in MVP) evaluates each group independently and fires per group (cap 20 groups per evaluation).

## Evaluation loop

- A scheduler ticks every 60 s and evaluates each enabled rule whose `last_evaluated_at + eval_interval ≤ now` (`eval_interval` = 1 min for windows ≤ 15 m, 5 min otherwise).
- Each evaluation is one DataFusion query over `[now − window, now]` (memtable included, so data is fresh), run with the alerts' own small memory pool.
- State machine per rule (per group when grouped):

```
ok ──(condition true)──► firing ──(condition false for 2 consecutive evaluations)──► ok
 │                         │
 └─(count < min_count)─► no_data (no notification)
```

- Transitions `ok→firing` and `firing→ok` create an `alert_events` row (`fired` / `resolved`) and notification deliveries. While firing, a reminder is sent every `cooldown` if `notify_while_firing = true` (default false).
- Evaluations are cheap enough to run sequentially; at most `alerts.concurrency` (default 4) in parallel.

## Channels

`notification_channels` (04), config encrypted.

| Kind | Config | Delivery |
|---|---|---|
| `slack` | `{webhook_url}` (Slack incoming webhook) | Block Kit message: rule name, state, value vs threshold, window, project, link to the filtered trace list |
| `webhook` | `{url, secret?, headers?}` | `POST` JSON (below), signed like the webhook evaluator (`X-Llmobs-Signature`) |
| `email` | `{recipients: [...]}` | via SMTP (08 §Email); disabled with a UI warning if SMTP is not configured |

Webhook payload:

```json
{"type": "alert.fired" | "alert.resolved",
 "rule": {"id", "name", "metric"}, "project": {"id", "name"},
 "value": 0.083, "threshold": 0.05, "comparator": ">", "window": "5m", "group": null,
 "fired_at": "2026-11-09T10:12:00Z", "url": "https://…/projects/…/traces?filter=…"}
```

Deliveries go through `notification_deliveries` with retries (backoff 30 s → 1 h, max 8 attempts). A "Send test notification" button per channel.

Online-judge rule pauses (budget, errors) can also notify: a built-in pseudo-alert on the project's default channels if any are configured.
