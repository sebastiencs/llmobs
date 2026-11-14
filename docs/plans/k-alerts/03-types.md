# K3 — Alert condition and channel config types, with validation

**Commit subject:** `server: add alert condition and channel config types`

Adds the types every later sub-phase shares, and their validation, with no database and no route:
- `crate::api::alerts`: the public enums and `AlertCondition` (they appear in the API and are stored as JSON in `meta.db`);
- `crate::alerts::condition`: `normalize_condition` (defaults and checks, D-K-2) and the mapping to K1's `AlertMeasure`;
- `crate::alerts::channel`: `ChannelConfig` (the sealed JSON of a channel, D-K-6), its validation and its `target_hint`.

## Read first

- README § Design, D-K-2, D-K-3, D-K-4, D-K-6.
- `crates/server/src/api/keys.rs` (derive and doc style of API types), `crates/server/src/api/common.rs` (`FieldError`), `crates/server/src/api/mod.rs`, `crates/server/src/lib.rs`.
- `crates/storage/src/reads/alerts.rs` (K1: `AlertMeasure`).

## Files

| File | Change |
|---|---|
| `crates/server/src/api/alerts.rs` | new |
| `crates/server/src/api/mod.rs` | `pub mod alerts;` (after `pub mod admin;`, before `pub mod common;`: rustfmt orders modules) |
| `crates/server/src/alerts/mod.rs` | new |
| `crates/server/src/alerts/condition.rs` | new |
| `crates/server/src/alerts/channel.rs` | new |
| `crates/server/src/lib.rs` | `pub(crate) mod alerts;` (alphabetically: after `pub mod admin;`, before `pub mod api;`) |

## 1. `crates/server/src/api/alerts.rs`

```rust
//! Alert rule, event and notification channel types.
//!
//! Spec 07 and spec 09 § Evaluation & alerts.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a rule measures (D-K-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertMetric {
    /// Traces with an error span divided by traces.
    ErrorRate,
    /// Sum of trace cost over the window, USD.
    Cost,
    /// p95 latency in milliseconds, of traces or of `llm` spans.
    LatencyP95,
    /// Average of a score, or the pass rate of a boolean score.
    Score,
}

impl AlertMetric {
    /// The stored spelling: `error_rate`, `cost`, `latency_p95`, `score`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ErrorRate => "error_rate",
            Self::Cost => "cost",
            Self::LatencyP95 => "latency_p95",
            Self::Score => "score",
        }
    }
}

/// Sliding window length (D-K-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum AlertWindow {
    /// One minute.
    #[serde(rename = "1m")]
    M1,
    /// Five minutes.
    #[serde(rename = "5m")]
    M5,
    /// Fifteen minutes.
    #[serde(rename = "15m")]
    M15,
    /// One hour.
    #[serde(rename = "1h")]
    H1,
    /// Six hours.
    #[serde(rename = "6h")]
    H6,
    /// One day.
    #[serde(rename = "24h")]
    H24,
}

impl AlertWindow {
    /// Window length in nanoseconds.
    #[must_use]
    pub fn ns(self) -> i64 {
        const MIN: i64 = 60_000_000_000;
        match self {
            Self::M1 => MIN,
            Self::M5 => 5 * MIN,
            Self::M15 => 15 * MIN,
            Self::H1 => 60 * MIN,
            Self::H6 => 360 * MIN,
            Self::H24 => 1_440 * MIN,
        }
    }

    /// Time between two evaluations: 60 s up to 15 minutes, else 300 s (D-K-3).
    #[must_use]
    pub fn interval_ns(self) -> i64 {
        match self {
            Self::M1 | Self::M5 | Self::M15 => 60_000_000_000,
            Self::H1 | Self::H6 | Self::H24 => 300_000_000_000,
        }
    }

    /// The JSON spelling: `1m`, `5m`, `15m`, `1h`, `6h`, `24h`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::H6 => "6h",
            Self::H24 => "24h",
        }
    }
}

/// How the value is compared with the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    /// `value > threshold`.
    Gt,
    /// `value >= threshold`.
    Gte,
    /// `value < threshold`.
    Lt,
    /// `value <= threshold`.
    Lte,
}

impl Comparator {
    /// Whether `value` breaches `threshold`.
    #[must_use]
    pub fn breaches(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Gt => value > threshold,
            Self::Gte => value >= threshold,
            Self::Lt => value < threshold,
            Self::Lte => value <= threshold,
        }
    }

    /// `>`, `>=`, `<` or `<=`, for messages.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
        }
    }
}

/// Which latency a `latency_p95` rule measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatencyTarget {
    /// Whole traces.
    Trace,
    /// `llm` spans.
    LlmSpan,
}

/// How a `score` rule aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreAggregation {
    /// Average of numeric, boolean (1/0) and mapped categorical values.
    Avg,
    /// Share of `true` among boolean scores.
    PassRate,
}

/// Where a rule stands (D-K-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// The last evaluation did not breach.
    Ok,
    /// The rule fired and has not resolved.
    Firing,
    /// Too few samples in the window.
    NoData,
}

impl AlertState {
    /// The stored spelling: `ok`, `firing`, `no_data`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Firing => "firing",
            Self::NoData => "no_data",
        }
    }

    /// Parses the stored spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Self::Ok),
            "firing" => Some(Self::Firing),
            "no_data" => Some(Self::NoData),
            _ => None,
        }
    }
}

/// A notification channel's kind (D-K-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    /// Slack incoming webhook.
    Slack,
    /// Signed HTTP POST.
    Webhook,
    /// Email through SMTP.
    Email,
}

impl ChannelKind {
    /// The stored spelling: `slack`, `webhook`, `email`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Webhook => "webhook",
            Self::Email => "email",
        }
    }

    /// Parses the stored spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "slack" => Some(Self::Slack),
            "webhook" => Some(Self::Webhook),
            "email" => Some(Self::Email),
            _ => None,
        }
    }
}

/// What a rule measures and when it breaches (D-K-2).
///
/// Fields that do not apply to `metric` must be absent. A stored or returned
/// condition always has its defaults filled in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AlertCondition {
    /// What is measured.
    pub metric: AlertMetric,
    /// Sliding window.
    pub window: AlertWindow,
    /// Filter DSL expression over `traces`. Absent means every trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// How the value is compared with `threshold`.
    pub comparator: Comparator,
    /// Fraction for `error_rate` and `pass_rate`, USD for `cost`, milliseconds for `latency_p95`.
    pub threshold: f64,
    /// Fewer samples than this gives `no_data`. Not for `cost`. Defaults: 20, or 10 for `score`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_count: Option<i64>,
    /// `latency_p95` only. Default `trace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<LatencyTarget>,
    /// `score` only: the score name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_name: Option<String>,
    /// `score` only. Default `avg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregation: Option<ScoreAggregation>,
}
```

`AlertCondition` derives no `Eq` (it holds an `f64`).

## 2. `crates/server/src/alerts/mod.rs`

```rust
//! Alerting: conditions, channels, evaluation and delivery (spec 07, track K).
#![cfg_attr(not(test), allow(dead_code))] // the routes use this from K5 on, the loops from K9 on

pub(crate) mod channel;
pub(crate) mod condition;
```

Later sub-phases add `pub(crate) mod evaluate;`, `message;`, `deliver;` here.

## 3. `crates/server/src/alerts/condition.rs`

```rust
//! Rule condition defaults and checks (D-K-2).

use llmobs_storage::AlertMeasure;

use crate::api::alerts::{AlertCondition, AlertMetric, LatencyTarget, ScoreAggregation};
use crate::api::common::FieldError;

/// Longest filter, in characters (the UI's `FILTER_MAX`).
pub(crate) const MAX_FILTER_CHARS: usize = 4096;
/// Longest score name, in characters.
pub(crate) const MAX_SCORE_NAME_CHARS: usize = 200;
/// Largest `min_count`.
pub(crate) const MAX_MIN_COUNT: i64 = 1_000_000;

/// Fills the defaults and checks every field. Errors name the field as
/// `condition.<name>`. Several errors are returned together, in field order.
pub(crate) fn normalize_condition(condition: AlertCondition) -> Result<AlertCondition, Vec<FieldError>>;

/// The storage measure for a normalized condition.
pub(crate) fn measure(condition: &AlertCondition) -> AlertMeasure;
```

### `normalize_condition` rules, in this order

Take the argument as `mut condition`, change its fields in place and return `Ok(condition)` (do not rebuild it from clones). Build `let mut errors = Vec::new();` and a `fn error(field: &str, message: &str) -> FieldError` helper that sets `field: format!("condition.{field}")`, `message`, `position: None`.

1. **filter**: trim it (`str::trim`). Empty after trimming → `None`. More than `MAX_FILTER_CHARS` characters (`chars().count()`) → error `filter`, `"at most 4096 characters"`. The trimmed text is what is stored. (Its syntax is checked by storage in the route, K6.)
2. **threshold**: not finite (`!threshold.is_finite()`) → error `threshold`, `"must be a finite number"`, and skip the per-metric check. Otherwise, per metric:
   - `error_rate`: outside `[0, 1]` → `"must be between 0 and 1"`;
   - `cost`: below 0 → `"must be at least 0"`;
   - `latency_p95`: `<= 0` → `"must be greater than 0"`;
   - `score` with aggregation `pass_rate` (after the default below): outside `[0, 1]` → `"must be between 0 and 1"`; with `avg`: no range.
3. **min_count**:
   - `cost`: `Some(_)` → error `min_count`, `"does not apply to cost"`; stays `None`.
   - other metrics: `None` → default `20` (`error_rate`, `latency_p95`) or `10` (`score`). `Some(n)` with `n < 1` or `n > MAX_MIN_COUNT` → `"must be between 1 and 1000000"`.
4. **target**: `latency_p95`: `None` → `Some(LatencyTarget::Trace)`. Other metrics: `Some(_)` → error `target`, `"applies to latency_p95 only"`.
5. **score_name**: `score`: trim; `None` or empty → error `score_name`, `"is required for score"`; more than `MAX_SCORE_NAME_CHARS` characters → `"at most 200 characters"`; store the trimmed text. Other metrics: `Some(_)` → error `score_name`, `"applies to score only"`.
6. **aggregation**: `score`: `None` → `Some(ScoreAggregation::Avg)`. Other metrics: `Some(_)` → error `aggregation`, `"applies to score only"`.

Compute the effective aggregation first, without pushing any error (`let aggregation = condition.aggregation.unwrap_or(ScoreAggregation::Avg);` for `score`), because step 2 needs it. Then push errors strictly in step order 1 to 6 (test 7 relies on it). Return `Err(errors)` when any error was pushed, else the normalized condition. `window` and `comparator` need no check (serde already restricts them).

### `measure`

```text
error_rate                         → AlertMeasure::ErrorRate
cost                               → AlertMeasure::Cost
latency_p95, target trace or None  → AlertMeasure::TraceLatencyP95
latency_p95, target llm_span       → AlertMeasure::LlmLatencyP95
score, aggregation pass_rate       → AlertMeasure::ScorePassRate { name }
score, aggregation avg or None     → AlertMeasure::ScoreAvg { name }
```

`name` is `score_name.clone().unwrap_or_default()` (a normalized `score` condition always has one).

### Tests (`#[cfg(test)] mod tests`)

Build conditions with a helper `fn condition(metric: AlertMetric) -> AlertCondition` that sets `window: AlertWindow::M5, filter: None, comparator: Comparator::Gt, threshold: 0.5, min_count: None, target: None, score_name: None, aggregation: None`, then adjust fields.

1. `defaults_are_filled`: `error_rate` → `min_count Some(20)`; `latency_p95` (threshold 100.0) → `min_count Some(20)`, `target Some(Trace)`; `score` with `score_name Some(" helpfulness ")` → `min_count Some(10)`, `aggregation Some(Avg)`, `score_name Some("helpfulness")`; `cost` → `min_count None`.
2. `filter_is_trimmed_and_empty_is_none`: `"  error_count > 0 "` → `Some("error_count > 0")`; `"   "` → `None`; 4 097 `a`s → one error with field `condition.filter`.
3. `thresholds_are_checked_per_metric`: every `score` case sets `score_name: Some("s".to_owned())`. Each case asserts the exact list of error fields (empty for ok): `error_rate` 1.5 → `[condition.threshold]`; `cost` −1.0 → `[condition.threshold]`; `latency_p95` 0.0 → `[condition.threshold]`; `score` + `pass_rate` 1.2 → `[condition.threshold]`; `score` + `avg` −3.0 → `[]`; `error_rate` `f64::NAN` → exactly one error, `condition.threshold` with message `"must be a finite number"`.
4. `fields_that_do_not_apply_are_refused`: `cost` with `min_count Some(5)` → `condition.min_count`; `error_rate` with `target Some(LlmSpan)` → `condition.target`; `error_rate` with `score_name Some("x")` → `condition.score_name`; `cost` with `aggregation Some(Avg)` → `condition.aggregation`.
5. `score_needs_a_name`: `score` with no name → `condition.score_name` `"is required for score"`.
6. `min_count_range`: 0 and 1 000 001 → error; 1 and 1 000 000 → ok.
7. `errors_are_collected_together`: `error_rate`, threshold 2.0, `target Some(Trace)` → exactly two errors, `condition.threshold` then `condition.target`.
8. `measure_follows_the_condition`: one assertion per row of the mapping table.
9. `condition_json_round_trips`: `serde_json::from_value::<AlertCondition>(json!({"metric":"score","window":"1h","comparator":"lt","threshold":0.7,"score_name":"helpfulness","aggregation":"pass_rate","min_count":10}))` succeeds; serializing it back gives the same JSON; an unknown field `"group_by"` fails to deserialize; window `"2h"` fails.
10. `window_interval`: `AlertMetric::LatencyP95.as_str()` is `"latency_p95"`; `M15.interval_ns()` is 60 s, `H1.interval_ns()` is 300 s; `H24.ns()` is 86 400 s; `M5.as_str()` is `"5m"`.
11. `comparator_breaches`: `Gt.breaches(1.0, 1.0)` false, `Gt.breaches(1.1, 1.0)` true, `Gte.breaches(1.0, 1.0)` true, `Lt.breaches(0.5, 1.0)` true, `Lt.breaches(1.0, 1.0)` false, `Lte.breaches(1.0, 1.0)` true, `Lte.breaches(1.1, 1.0)` false.

## 4. `crates/server/src/alerts/channel.rs`

```rust
//! A channel's sealed configuration (D-K-6). K4 seals it with `crypto::SecretBox`.

use std::collections::BTreeMap;
use std::fmt;

use reqwest::header::{HeaderName, HeaderValue};
use url::Url;
use serde::{Deserialize, Serialize};

use crate::api::alerts::ChannelKind;
use crate::api::common::FieldError;

/// Most extra webhook headers.
pub(crate) const MAX_HEADERS: usize = 10;
/// Most email recipients.
pub(crate) const MAX_RECIPIENTS: usize = 20;
/// Longest URL, in bytes.
pub(crate) const MAX_URL_BYTES: usize = 2048;
/// Longest header value, in bytes.
pub(crate) const MAX_HEADER_VALUE_BYTES: usize = 1024;
/// Header names the server sets itself; a channel may not set them (lower case).
pub(crate) const RESERVED_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "host",
    "user-agent",
    "x-llmobs-signature",
    "x-llmobs-delivery",
];

/// The decrypted configuration of one channel. Never logged: [`fmt::Debug`] prints the kind only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ChannelConfig {
    /// Slack incoming webhook URL.
    Slack { url: String },
    /// Signed POST.
    Webhook {
        url: String,
        secret: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    /// Recipients, as given (trimmed).
    Email { recipients: Vec<String> },
}

impl fmt::Debug for ChannelConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ChannelConfig({})", self.kind().as_str())
    }
}

impl ChannelConfig {
    /// The channel kind.
    pub(crate) fn kind(&self) -> ChannelKind;
    /// Checks every field; errors are returned together, named `url`, `secret`, `headers`,
    /// `recipients` (the request field names, K5).
    pub(crate) fn validate(&self) -> Result<(), Vec<FieldError>>;
    /// What responses show instead of the config (D-K-6).
    pub(crate) fn target_hint(&self) -> String;
}

/// `true` when `text` is a single email address (`lettre::Address`).
pub(crate) fn is_email_address(text: &str) -> bool {
    text.parse::<lettre::Address>().is_ok()
}
```


### `validate` rules

Field errors use `position: None`. For each kind:

- **Slack**: `url` must parse with `Url::parse`, have scheme `https` and a host, and be at most `MAX_URL_BYTES` bytes. Messages: `"must be a valid URL"`, `"must use https"`, `"at most 2048 bytes"`. One error at most for `url` (the first that applies, in this order: length, parse, scheme/host — use `"must be a valid URL"` for a missing host).
- **Webhook**:
  - `url`: as Slack, in the same order (length, parse, scheme/host), but the scheme may be `http` or `https` (`"must use http or https"`).
  - `secret`: 16 to 256 characters (`chars().count()`), else `"must be 16 to 256 characters"`.
  - `headers`: more than `MAX_HEADERS` entries → `"at most 10 headers"`. For each `(name, value)`: `HeaderName::from_bytes(name.as_bytes())` fails → `"invalid header name"`; the lower-cased name is in `RESERVED_HEADERS` → `"header is set by the server"`; `value.len() > MAX_HEADER_VALUE_BYTES` or `HeaderValue::from_str(value)` fails → `"invalid header value"`. Report one `headers` error at most (the first found, iterating the `BTreeMap` in order); the message must not contain the header value.
- **Email**: one `recipients` error at most, the first that applies in this order: empty → `"at least one recipient"`; more than `MAX_RECIPIENTS` → `"at most 20 recipients"`; the first entry that is not `is_email_address(entry)` → `"invalid email address"`; two entries equal ignoring ASCII case → `"duplicate recipient"`. The message never contains the address.

### `target_hint`

- **Slack**: `"{host}/…/{last4}"` where `host` is the URL's host (`Url::host_str`, empty when it does not parse) and `last4` is the last 4 `char`s of the whole URL string (`let chars: Vec<char> = url.chars().collect(); chars[chars.len().saturating_sub(4)..].iter().collect::<String>()`; never slice bytes, which can panic on multi-byte input). Example: `https://hooks.slack.com/services/T000/B000/XXXXabcd` → `hooks.slack.com/…/abcd`. The `…` is U+2026.
- **Webhook**: the host, plus `:port` when the URL has an explicit port (`Url::port`); the empty string when the URL does not parse. Example: `https://hooks.example.com:8443/x?y` → `hooks.example.com:8443`.
- **Email**: the first recipient; when there are more, append `" +N"` with N the count of the others. Example: `["a@x.io","b@x.io","c@x.io"]` → `a@x.io +2`.

### Tests (`#[cfg(test)] mod tests`)

1. `slack_url_must_be_https`: `http://hooks.slack.com/x` → one `url` error `"must use https"`; `not a url` → `"must be a valid URL"`; `format!("https://hooks.slack.com/{}", "a".repeat(2049 - 24))` (2 049 bytes) → `"at most 2048 bytes"`; `https://hooks.slack.com/services/T/B/abcd` → ok.
2. `webhook_checks_secret_and_headers`: secret of 15 characters → `secret` error; 16 → ok; 257 → error. Headers: 11 entries → `"at most 10 headers"`; `{"X-Llmobs-Signature": "v"}` → `"header is set by the server"`; `{"bad name": "v"}` → `"invalid header name"`; `{"X-Ok": "line\nbreak"}` → `"invalid header value"`, and the message does not contain `"line"`; `{"X-Team": "core"}` → ok.
3. `webhook_allows_http`: `http://10.0.0.1:9000/hook` validates (the egress guard decides at send time, not here).
4. `email_recipients`: `[]` → `"at least one recipient"`; 21 valid addresses (`(0..21).map(|i| format!("u{i}@x.io"))`) → `"at most 20 recipients"`; `["not-an-address"]` → `"invalid email address"`; `["A@x.io","a@x.io"]` → `"duplicate recipient"`; `["a@x.io"]` → ok.
5. `errors_are_collected_together`: a webhook with a bad URL and a short secret → two errors, `url` then `secret`.
6. `target_hints`: the three examples above, and a webhook URL without a port → host only.
7. `debug_hides_the_config`: `format!("{:?}", ChannelConfig::Webhook { url: "https://h.io/secret-path".into(), secret: "s".repeat(16), headers: BTreeMap::new() })` is exactly `ChannelConfig(webhook)`.
8. `config_json_round_trips`: `serde_json::to_value` of a webhook config is `{"kind":"webhook","url":…,"secret":…,"headers":{}}`; it deserializes back to an equal value; `{"kind":"sms"}` fails; `{"kind":"slack","url":"x","extra":1}` fails.
9. `email_addresses`: `is_email_address` accepts `A@x.io` and rejects `a@`, `a b@x.io` and `""`.

## Commands

```bash
cargo test -p llmobs-server alerts::
```

```bash
just check
```

`just check` includes the OpenAPI contract test. The new types are not registered in the document yet (K5 and K6 do it), so `crates/server/openapi.json` does not change in this sub-phase.

## Done when

- The modules and types above exist with exactly these names and signatures.
- The 20 tests pass (11 in `alerts::condition`, 9 in `alerts::channel`) and `just check` is green.
