# 07 — Alerts

Per-project alert rules over four metrics, evaluated in the background over sliding windows, with firing/resolved state and notifications to Slack, a signed webhook and email. Track K's plan (`docs/plans/k-alerts/`) and decisions D-K-1–D-K-16 give the details. Alerts are core (MIT) code (D-K-15).

## Rule model

`alert_rules` (04). The condition (`config`, JSON) is flat; fields that do not apply to the metric must be absent (D-K-2):

| `metric` | Fields | Computed over the window |
|---|---|---|
| `error_rate` | `filter?`, `window`, `comparator`, `threshold` (fraction 0–1), `min_count` (default 20) | traces with `error_count > 0` / traces |
| `cost` | `filter?`, `window`, `comparator`, `threshold` (USD ≥ 0) | `sum(cost_total)` on traces (0 when none) |
| `latency_p95` | `filter?`, `window`, `target` (`trace` default, or `llm_span`), `comparator`, `threshold` (ms > 0), `min_count` (default 20) | `approx_percentile_cont(duration_ns, 0.95)` in ms, of traces or of `llm` spans |
| `score` | `filter?`, `window`, `score_name`, `aggregation` (`avg` default, or `pass_rate`), `comparator`, `threshold` (`pass_rate`: 0–1), `min_count` (default 10) | `avg(value_num)` of live trace/span scores of that name; `pass_rate` keeps boolean scores only |

- `window` ∈ {`1m`, `5m`, `15m`, `1h`, `6h`, `24h`}; `comparator` ∈ {`gt`, `gte`, `lt`, `lte`}; `min_count` 1–1 000 000.
- `filter` is a **`traces`** filter-DSL expression for every metric; span and score measures keep rows whose trace passes it (as dashboard metrics, 05). Experiment traces are always excluded; evaluation traces (`environment = 'llmobs-eval'`) are excluded unless the filter names `environment` (05 § Default exclusions).
- Rule fields: `name` (1–100, unique per project), `enabled`, `channel_ids` (≤ 10 channels of the same project), `cooldown_s` (300–604 800, default 3 600), `notify_while_firing` (default false).
- Limits: 100 rules and 20 channels per project (D-K-5). On the cloud Free plan an org may have one enabled rule; creating or re-enabling past the plan limit is `403 plan_limit` (D178, D-K-14). Self-hosted has no limit.
- There is no `group_by` in the MVP: a per-model cost alert is a rule with a filter (D-K-2).

## Evaluation loop

- A tick every 60 s evaluates each enabled rule of an active project and org that was never evaluated or whose `last_evaluated_at + interval − 5 s ≤ now` (`interval` = 60 s for windows ≤ 15 m, 300 s otherwise) (D-K-3).
- Each evaluation is one aggregate query (`llmobs_storage::alert_value`) over `[now − window, now)` (memtable included) through the query context every read uses, so the plan's data-access window applies (D175, D-K-16). At most 4 evaluations run at once, each with a 30 s timeout; a tick waits for the previous one.
- An evaluation is *insufficient* when the sample count is below `min_count`, *breaching* when it is not insufficient, has a value, and `value <comparator> threshold`, else *clear*.

```
ok / no_data ──(breaching)──► firing ──(2 clear or insufficient in a row)──► ok (or no_data)
      │
      ├─(clear)────────► ok          (no notification)
      └─(insufficient)─► no_data     (no notification)
```

- `ok/no_data → firing` records a `fired` event; `firing → ok/no_data` a `resolved` event. While firing, a `reminder` event is recorded every `cooldown_s` when `notify_while_firing` is on. Each event creates one pending delivery per channel, in the same transaction.
- A failed query changes no state; it sets `last_error` and is never treated as no data.
- Changing a rule's condition, or disabling or re-enabling it, resets it to `ok` with no event (D-K-4). A write is fenced by `updated_at`, so an evaluation never overwrites a concurrent edit.

## Channels

`notification_channels` (04), per project; the whole config is one sealed JSON (`config_enc`, AAD `notification_channels:config_enc:<id>`). Responses never show a URL path, a secret or a header value; they show `target_hint` (D-K-6).

| Kind | Config | Delivery |
|---|---|---|
| `slack` | `{url}` (an `https` Slack incoming webhook) | Block Kit message: title, project, metric, value, threshold, window, samples, buttons to the rule and to the trace list |
| `webhook` | `{url, secret (16–256), headers?}` (≤ 10 headers) | `POST` JSON (below), `X-Llmobs-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>` (as the webhook evaluator), `X-Llmobs-Delivery: <delivery id>` |
| `email` | `{recipients}` (1–20) | the `alert` email template (08 § Email), one message per recipient; creating one needs SMTP, and existing ones show "SMTP not configured" without it |

Webhook payload (`type` is `alert.fired`, `alert.resolved`, `alert.reminder` or `alert.test`; `value` and `count` are `null` when absent):

```json
{"type": "alert.fired", "event_id": "…",
 "rule": {"id", "name", "metric"}, "project": {"id", "name"},
 "value": 0.083, "count": 120, "threshold": 0.05, "comparator": "gt", "window": "5m",
 "window_from": "…", "window_to": "…", "occurred_at": "…",
 "url": "https://…/<org>/<project>/traces?from=…&to=…&filter=…",
 "rule_url": "https://…/<org>/<project>/alerts/<rule id>"}
```

Deliveries go through `notification_deliveries`: a loop every 10 s sends due ones (at most 4 at once, 10 s timeout). Retryable failures (timeouts, connection errors, 408, 429, 5xx, SMTP connection, TLS and authentication errors) wait `min(30 s · 2^(attempts−1), 1 h)`, up to 8 attempts; other failures fail at once. Delivery is at least once. Channels are never disabled automatically: the UI shows each channel's last finished delivery, and "Send test" sends one sample message synchronously (D-K-7).

History: events are kept 90 days and finished deliveries 30 days, pruned hourly (D-K-10).

Online-judge rule pauses do not raise alerts; track I shows them as banners (D-K-12, D-I-14).
