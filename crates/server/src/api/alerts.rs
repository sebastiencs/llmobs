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
