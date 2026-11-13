//! Dashboard metrics.
//!
//! Spec 05 § Dashboard metrics. Latencies and TTFT are nanoseconds as `f64`. Costs are USD.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::common::TimeParam;

/// Query parameters of `GET /api/v1/projects/{project}/metrics`.
///
/// Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct MetricsParams {
    /// Inclusive range start. RFC 3339 or epoch milliseconds.
    pub from: TimeParam,
    /// Exclusive range end. RFC 3339 or epoch milliseconds.
    pub to: TimeParam,
    /// Bucket width. Defaults to `auto` (~120 points).
    #[serde(default)]
    pub bucket: MetricsBucket,
    /// Filter DSL expression. Applied to the view each series is computed on (spec 05).
    #[serde(default)]
    pub filter: Option<String>,
    /// Split series that are not already split.
    #[serde(default)]
    pub group_by: Option<MetricsGroupBy>,
    /// Include traces from the evaluation environment. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_eval: bool,
    /// Include experiment traces. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_experiments: bool,
}

/// Width of each metrics bucket.
///
/// Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricsBucket {
    /// About 120 buckets across the range.
    #[default]
    Auto,
    /// One minute.
    #[serde(rename = "1m")]
    M1,
    /// Five minutes.
    #[serde(rename = "5m")]
    M5,
    /// Ten minutes.
    #[serde(rename = "10m")]
    M10,
    /// Fifteen minutes.
    #[serde(rename = "15m")]
    M15,
    /// Thirty minutes.
    #[serde(rename = "30m")]
    M30,
    /// One hour.
    #[serde(rename = "1h")]
    H1,
    /// Six hours.
    #[serde(rename = "6h")]
    H6,
    /// Twelve hours.
    #[serde(rename = "12h")]
    H12,
    /// One day.
    #[serde(rename = "1d")]
    D1,
}

/// Dimension that splits series which are not already split.
///
/// Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricsGroupBy {
    /// Each model named on the trace.
    Model,
    /// `environment`.
    Environment,
    /// `release`.
    Release,
}

/// Dashboard response.
///
/// Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricsResponse {
    /// Inclusive range start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub from: i64,
    /// Exclusive range end, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub to: i64,
    /// Resolved bucket width, nanoseconds.
    pub bucket_ns: i64,
    /// Split applied to series that are not already split. Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<MetricsGroupBy>,
    /// Headline numbers for the range and the previous range of the same length.
    pub kpis: Vec<Kpi>,
    /// One point per bucket, for every series.
    pub series: Vec<MetricSeries>,
    /// At most 10 models, by cost descending.
    pub top_models: Vec<ModelUsage>,
}

/// One headline number.
///
/// `previous` covers the same-length period just before `from`. Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Kpi {
    /// Which headline.
    pub metric: KpiMetric,
    /// Value over `[from, to)`. Omitted when the range has no data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// Value over the previous range. Omitted when that range has no data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<f64>,
}

/// Headline metrics.
///
/// Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KpiMetric {
    /// `count(*)` on `traces`.
    Traces,
    /// Traces with `error_count > 0`, divided by the trace count.
    ErrorRate,
    /// `approx_percentile_cont(duration_ns, 0.95)` on `traces`, nanoseconds.
    LatencyP95,
    /// `sum(cost_total)` on `traces`, USD.
    CostTotal,
    /// Sum of `total_tokens` on `traces`.
    TotalTokens,
}

/// One time series.
///
/// Every series has one point per bucket from `from` to `to`. Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricSeries {
    /// Which series.
    pub metric: MetricName,
    /// Group key: the `group_by` value, a model, or a score name. Omitted when the series is not split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// One point per bucket.
    pub points: Vec<MetricPoint>,
}

/// One bucket of a series.
///
/// Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricPoint {
    /// Bucket start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub time: i64,
    /// Bucket value. Omitted when the bucket has no data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// Series name.
///
/// Latencies and TTFT are nanoseconds as `f64`. Costs are USD. Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricName {
    /// `count(*)` on `traces`. `group` is the `group_by` value, or absent.
    Traces,
    /// `sum(error_count > 0)` on `traces`. `group` is the `group_by` value, or absent.
    Errors,
    /// Errors divided by traces. `group` is the `group_by` value, or absent.
    ErrorRate,
    /// Trace `duration_ns` p50. `group` is the `group_by` value, or absent.
    LatencyP50,
    /// Trace `duration_ns` p95. `group` is the `group_by` value, or absent.
    LatencyP95,
    /// Trace `duration_ns` p99. `group` is the `group_by` value, or absent.
    LatencyP99,
    /// `sum(cost_total)` on `traces`, USD. `group` is the `group_by` value, or absent.
    Cost,
    /// Sum of `input_tokens` on `llm` and `embedding` spans. `group` is the `group_by` value, or absent.
    InputTokens,
    /// Sum of `output_tokens` on `llm` and `embedding` spans. `group` is the `group_by` value, or absent.
    OutputTokens,
    /// Sum of `cache_read_tokens` on `llm` and `embedding` spans. `group` is the `group_by` value, or absent.
    CacheReadTokens,
    /// Sum of `cache_write_tokens` on `llm` and `embedding` spans. `group` is the `group_by` value, or absent.
    CacheWriteTokens,
    /// `ttft_ns` p50 on `llm` spans. `group` is the `group_by` value, or absent.
    TtftP50,
    /// `ttft_ns` p95 on `llm` spans. `group` is the `group_by` value, or absent.
    TtftP95,
    /// `duration_ns` p50 on `llm` spans. `group` is the model.
    LlmLatencyP50,
    /// `duration_ns` p95 on `llm` spans. `group` is the model.
    LlmLatencyP95,
    /// `sum(cost_total)` on `llm` spans, USD. `group` is the model.
    CostByModel,
    /// `avg(value_num)` per score name. `group` is the score name.
    ScoreAvg,
    /// `sum(cost_total)` on evaluation traces (the reserved evaluation environment, spec 06), USD.
    /// `group` is absent.
    EvalCost,
}

/// One model in the cost ranking.
///
/// At most 10, by cost descending. Spec 05 § Dashboard metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ModelUsage {
    /// Model name.
    pub model: String,
    /// Number of `llm` spans.
    pub llm_count: i64,
    /// Sum of `total_tokens`. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Sum of `cost_total`, USD. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::{
        Kpi, KpiMetric, MetricName, MetricPoint, MetricSeries, MetricsBucket, MetricsParams,
        MetricsResponse, ModelUsage,
    };

    const AT: i64 = 1_767_261_600_000_000_000;

    #[test]
    fn metrics_bucket_spellings_round_trip() {
        let spellings = [
            (MetricsBucket::Auto, "auto"),
            (MetricsBucket::M1, "1m"),
            (MetricsBucket::M5, "5m"),
            (MetricsBucket::M10, "10m"),
            (MetricsBucket::M15, "15m"),
            (MetricsBucket::M30, "30m"),
            (MetricsBucket::H1, "1h"),
            (MetricsBucket::H6, "6h"),
            (MetricsBucket::H12, "12h"),
            (MetricsBucket::D1, "1d"),
        ];
        for (bucket, spelling) in spellings {
            let json = serde_json::to_string(&bucket).unwrap();
            assert_eq!(json, format!("\"{spelling}\""));
            assert_eq!(
                serde_json::from_str::<MetricsBucket>(&json).unwrap(),
                bucket
            );
        }
    }

    #[test]
    fn metrics_params_require_from() {
        assert!(serde_urlencoded::from_str::<MetricsParams>("to=2026-01-01T11:00:00Z").is_err());
        let params: MetricsParams =
            serde_urlencoded::from_str("from=2026-01-01T10:00:00Z&to=2026-01-01T11:00:00Z")
                .unwrap();
        assert_eq!(params.bucket, MetricsBucket::Auto);
        assert_eq!(params.from.ns(), AT);
    }

    #[test]
    fn metrics_response_snapshot() {
        let response = MetricsResponse {
            from: AT,
            to: AT + 3_600_000_000_000,
            bucket_ns: 600_000_000_000,
            group_by: None,
            kpis: vec![Kpi {
                metric: KpiMetric::Traces,
                value: Some(4.0),
                previous: None,
            }],
            series: vec![MetricSeries {
                metric: MetricName::Traces,
                group: None,
                points: vec![MetricPoint {
                    time: AT,
                    value: Some(2.0),
                }],
            }],
            top_models: vec![ModelUsage {
                model: "claude-sonnet-4-5".to_owned(),
                llm_count: 3,
                total_tokens: Some(3100),
                cost_total: Some(0.00576),
            }],
        };

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(
                "metrics_response",
                serde_json::to_string_pretty(&response).unwrap()
            );
        });
    }
}
