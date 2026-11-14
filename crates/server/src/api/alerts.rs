//! Alert rule, event and notification channel types.
//!
//! Spec 07 and spec 09 § Evaluation & alerts.

use std::collections::BTreeMap;

use llmobs_core::time_serde;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

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

/// A delivery's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// Waiting for its first or next attempt.
    Pending,
    /// The channel accepted it.
    Sent,
    /// Gave up: a permanent error, or 8 attempts.
    Failed,
}

impl DeliveryStatus {
    /// The stored spelling: `pending`, `sent`, `failed`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    /// Parses the stored spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "sent" => Some(Self::Sent),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// The newest finished delivery of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelDelivery {
    /// `sent` or `failed`.
    pub status: DeliveryStatus,
    /// When it finished.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub at: i64,
    /// Why it failed. Never contains a URL, a secret or an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A notification channel. The URL, the secret and header values are never returned (D-K-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotificationChannel {
    /// Channel id.
    pub id: Uuid,
    /// Display name, unique in the project.
    pub name: String,
    /// `slack`, `webhook` or `email`.
    pub kind: ChannelKind,
    /// Where it sends, without secrets: `hooks.slack.com/…/abcd`, a host, or the first address and `+N`.
    pub target_hint: String,
    /// False for an email channel while SMTP is not configured.
    pub available: bool,
    /// Email recipients. Empty for other kinds.
    pub recipients: Vec<String>,
    /// Names of the extra webhook headers. Empty for other kinds.
    pub header_names: Vec<String>,
    /// The newest finished delivery. Absent when none finished yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<ChannelDelivery>,
    /// Creation time.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last change.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// Body of `POST /channels`. Send the fields of `kind` only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateChannelRequest {
    /// Display name, 1–100 characters after trimming.
    pub name: String,
    /// The kind. Cannot change later.
    pub kind: ChannelKind,
    /// `slack` and `webhook`: the URL.
    #[serde(default)]
    pub url: Option<String>,
    /// `webhook`: the signing secret, 16–256 characters.
    #[serde(default)]
    pub secret: Option<String>,
    /// `webhook`: extra headers, at most 10.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// `email`: 1–20 addresses.
    #[serde(default)]
    pub recipients: Option<Vec<String>>,
}

/// Body of `PATCH /channels/{id}`. An absent field keeps its value; `headers` replaces the whole map.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChannelRequest {
    /// New display name.
    #[serde(default)]
    pub name: Option<String>,
    /// `slack` and `webhook`: a new URL.
    #[serde(default)]
    pub url: Option<String>,
    /// `webhook`: a new signing secret.
    #[serde(default)]
    pub secret: Option<String>,
    /// `webhook`: the new header map.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// `email`: the new recipient list.
    #[serde(default)]
    pub recipients: Option<Vec<String>>,
}
