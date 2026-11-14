# Alerting in competitors

Notes for track K (`docs/plans/k-alerts/`), written 2026-11-14 from the source trees in `/home/sebastien/github/`. They compare designs only; no code is copied.

| | Helicone | Opik | LangWatch | Laminar | Phoenix |
|---|---|---|---|---|---|
| Rule kind | metric threshold | metric threshold, plus event triggers | saved-graph threshold, plus trace automations | events (LLM "signals", new clusters) | none |
| Metrics | error rate, cost, latency, tokens, request count | cost, latency, error count, avg feedback score | any custom graph series | — | — |
| Scope | org | workspace (optional project) | project | project | — |
| Evaluation | cron every 1 min, one ClickHouse query per rule, sliding `now − window` | one job every 5 min, sliding window, Redis lock per alert | on every ingested event, plus a 30 s sweep for no-data and resolves | queue per event | — |
| State | `triggered` / `resolved`, history rows; resolves after 5 min under threshold | none: re-fires every run, never resolves | open / resolved incident, fire dedup per destination | send lock per alert | — |
| Min sample | `minimum_request_count`, checked only when entering triggered | — | — | — | — |
| Channels | email (Resend), Slack app | webhook (`Authorization: Bearer` secret), Slack incoming webhook, PagerDuty | email with caps, Slack bot or incoming webhook, HMAC-signed webhook with event id | Slack app, email | — |
| Delivery | no retries, errors logged only | Redis stream, 3 attempts, 500 ms–30 s backoff, SSRF guard, test send | outbox with retries, `Retry-After`, test fire | bounded email retry | — |

## Lessons for K

1. Keep explicit per-rule state and history, and notify on transitions only (Helicone, LangWatch). Opik re-notifies every 5 minutes and never says when a problem is over.
2. Resolve with hysteresis, and send the resolve notification. LangWatch closes incidents silently.
3. If alerts are grouped, state must be per group. Helicone keeps one state per grouped rule, so a second group that breaches while the first fires says nothing. K drops `group_by` from the MVP instead (D-K-2).
4. Check the minimum sample on every evaluation, not only on entry. A failed query is not "no data": LangWatch treats a failed recency read as absence, which gives false alerts and false resolves.
5. Validate configs on write, with one unit per field and one spelling per operator. Opik had alerts that stayed unparseable forever (operator spelling) and silently skipped configs with no window.
6. Webhooks: HMAC signature, a stable event id for receiver dedup, bounded retries, SSRF guard, no redirects, test send.
7. Make delivery failures visible in the UI. Helicone only logs them.
8. A global ~1-minute tick over sliding windows is the common design (Helicone, Opik at 5 min). Tumbling windows appear nowhere.
