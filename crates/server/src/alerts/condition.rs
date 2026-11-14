//! Rule condition defaults and checks (D-K-2).

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
pub(crate) fn normalize_condition(
    mut condition: AlertCondition,
) -> Result<AlertCondition, Vec<FieldError>> {
    let mut errors = Vec::new();

    // 1. filter
    let filter = condition
        .filter
        .take()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    if filter
        .as_ref()
        .is_some_and(|text| text.chars().count() > MAX_FILTER_CHARS)
    {
        errors.push(error("filter", "at most 4096 characters"));
    }
    condition.filter = filter;

    // 2. threshold. The effective aggregation is needed here, before any error is pushed.
    let aggregation = condition.aggregation.unwrap_or(ScoreAggregation::Avg);
    let threshold = condition.threshold;
    if threshold.is_finite() {
        let problem = match condition.metric {
            AlertMetric::ErrorRate => {
                (!(0.0..=1.0).contains(&threshold)).then_some("must be between 0 and 1")
            }
            AlertMetric::Cost => (threshold < 0.0).then_some("must be at least 0"),
            AlertMetric::LatencyP95 => (threshold <= 0.0).then_some("must be greater than 0"),
            AlertMetric::Score => (aggregation == ScoreAggregation::PassRate
                && !(0.0..=1.0).contains(&threshold))
            .then_some("must be between 0 and 1"),
        };
        if let Some(message) = problem {
            errors.push(error("threshold", message));
        }
    } else {
        errors.push(error("threshold", "must be a finite number"));
    }

    // 3. min_count
    match condition.metric {
        AlertMetric::Cost => {
            if condition.min_count.is_some() {
                errors.push(error("min_count", "does not apply to cost"));
            }
            condition.min_count = None;
        }
        metric => {
            let default = if metric == AlertMetric::Score { 10 } else { 20 };
            let min_count = condition.min_count.unwrap_or(default);
            if !(1..=MAX_MIN_COUNT).contains(&min_count) {
                errors.push(error("min_count", "must be between 1 and 1000000"));
            }
            condition.min_count = Some(min_count);
        }
    }

    // 4. target
    if condition.metric == AlertMetric::LatencyP95 {
        condition.target = Some(condition.target.unwrap_or(LatencyTarget::Trace));
    } else if condition.target.is_some() {
        errors.push(error("target", "applies to latency_p95 only"));
    }

    // 5. score_name
    if condition.metric == AlertMetric::Score {
        let name = condition
            .score_name
            .take()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        match name {
            None => errors.push(error("score_name", "is required for score")),
            Some(name) => {
                if name.chars().count() > MAX_SCORE_NAME_CHARS {
                    errors.push(error("score_name", "at most 200 characters"));
                }
                condition.score_name = Some(name);
            }
        }
    } else if condition.score_name.is_some() {
        errors.push(error("score_name", "applies to score only"));
    }

    // 6. aggregation
    if condition.metric == AlertMetric::Score {
        condition.aggregation = Some(aggregation);
    } else if condition.aggregation.is_some() {
        errors.push(error("aggregation", "applies to score only"));
    }

    if errors.is_empty() {
        Ok(condition)
    } else {
        Err(errors)
    }
}

fn error(field: &str, message: &str) -> FieldError {
    FieldError {
        field: format!("condition.{field}"),
        message: message.to_owned(),
        position: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::alerts::{AlertWindow, Comparator};
    use serde_json::json;

    fn condition(metric: AlertMetric) -> AlertCondition {
        AlertCondition {
            metric,
            window: AlertWindow::M5,
            filter: None,
            comparator: Comparator::Gt,
            threshold: 0.5,
            min_count: None,
            target: None,
            score_name: None,
            aggregation: None,
        }
    }

    /// The field names of every error, in order; empty when the condition is valid.
    fn fields(condition: AlertCondition) -> Vec<String> {
        match normalize_condition(condition) {
            Ok(_) => Vec::new(),
            Err(errors) => errors.into_iter().map(|error| error.field).collect(),
        }
    }

    #[test]
    fn defaults_are_filled() {
        let rate = normalize_condition(condition(AlertMetric::ErrorRate)).expect("valid");
        assert_eq!(rate.min_count, Some(20));

        let mut latency = condition(AlertMetric::LatencyP95);
        latency.threshold = 100.0;
        let latency = normalize_condition(latency).expect("valid");
        assert_eq!(latency.min_count, Some(20));
        assert_eq!(latency.target, Some(LatencyTarget::Trace));

        let mut score = condition(AlertMetric::Score);
        score.score_name = Some(" helpfulness ".to_owned());
        let score = normalize_condition(score).expect("valid");
        assert_eq!(score.min_count, Some(10));
        assert_eq!(score.aggregation, Some(ScoreAggregation::Avg));
        assert_eq!(score.score_name.as_deref(), Some("helpfulness"));

        let cost = normalize_condition(condition(AlertMetric::Cost)).expect("valid");
        assert_eq!(cost.min_count, None);
    }

    #[test]
    fn filter_is_trimmed_and_empty_is_none() {
        let mut filter = condition(AlertMetric::ErrorRate);
        filter.filter = Some("  error_count > 0 ".to_owned());
        let filter = normalize_condition(filter).expect("valid");
        assert_eq!(filter.filter.as_deref(), Some("error_count > 0"));

        let mut blank = condition(AlertMetric::ErrorRate);
        blank.filter = Some("   ".to_owned());
        let blank = normalize_condition(blank).expect("valid");
        assert_eq!(blank.filter, None);

        let mut long = condition(AlertMetric::ErrorRate);
        long.filter = Some("a".repeat(4097));
        assert_eq!(fields(long), ["condition.filter"]);
    }

    #[test]
    fn thresholds_are_checked_per_metric() {
        let mut rate = condition(AlertMetric::ErrorRate);
        rate.threshold = 1.5;
        assert_eq!(fields(rate), ["condition.threshold"]);

        let mut cost = condition(AlertMetric::Cost);
        cost.threshold = -1.0;
        assert_eq!(fields(cost), ["condition.threshold"]);

        let mut latency = condition(AlertMetric::LatencyP95);
        latency.threshold = 0.0;
        assert_eq!(fields(latency), ["condition.threshold"]);

        let mut pass_rate = condition(AlertMetric::Score);
        pass_rate.score_name = Some("s".to_owned());
        pass_rate.aggregation = Some(ScoreAggregation::PassRate);
        pass_rate.threshold = 1.2;
        assert_eq!(fields(pass_rate), ["condition.threshold"]);

        let mut avg = condition(AlertMetric::Score);
        avg.score_name = Some("s".to_owned());
        avg.threshold = -3.0;
        assert_eq!(fields(avg), Vec::<String>::new());

        let mut nan = condition(AlertMetric::ErrorRate);
        nan.threshold = f64::NAN;
        let errors = normalize_condition(nan).expect_err("NAN is refused");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "condition.threshold");
        assert_eq!(errors[0].message, "must be a finite number");
    }

    #[test]
    fn fields_that_do_not_apply_are_refused() {
        let mut cost = condition(AlertMetric::Cost);
        cost.min_count = Some(5);
        assert_eq!(fields(cost), ["condition.min_count"]);

        let mut rate = condition(AlertMetric::ErrorRate);
        rate.target = Some(LatencyTarget::LlmSpan);
        assert_eq!(fields(rate), ["condition.target"]);

        let mut named = condition(AlertMetric::ErrorRate);
        named.score_name = Some("x".to_owned());
        assert_eq!(fields(named), ["condition.score_name"]);

        let mut cost_aggregation = condition(AlertMetric::Cost);
        cost_aggregation.aggregation = Some(ScoreAggregation::Avg);
        assert_eq!(fields(cost_aggregation), ["condition.aggregation"]);
    }

    #[test]
    fn score_needs_a_name() {
        let errors = normalize_condition(condition(AlertMetric::Score)).expect_err("no name");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "condition.score_name");
        assert_eq!(errors[0].message, "is required for score");
    }

    #[test]
    fn min_count_range() {
        let with_min = |min_count: i64| {
            let mut rule = condition(AlertMetric::ErrorRate);
            rule.min_count = Some(min_count);
            rule
        };
        assert_eq!(fields(with_min(0)), ["condition.min_count"]);
        assert_eq!(fields(with_min(1_000_001)), ["condition.min_count"]);
        assert!(normalize_condition(with_min(1)).is_ok());
        assert!(normalize_condition(with_min(1_000_000)).is_ok());
    }

    #[test]
    fn errors_are_collected_together() {
        let mut rule = condition(AlertMetric::ErrorRate);
        rule.threshold = 2.0;
        rule.target = Some(LatencyTarget::Trace);
        assert_eq!(fields(rule), ["condition.threshold", "condition.target"]);
    }

    #[test]
    fn condition_json_round_trips() {
        let value = json!({
            "metric": "score",
            "window": "1h",
            "comparator": "lt",
            "threshold": 0.7,
            "score_name": "helpfulness",
            "aggregation": "pass_rate",
            "min_count": 10
        });
        let parsed: AlertCondition = serde_json::from_value(value.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&parsed).expect("serializes"), value);

        let mut with_group = value.clone();
        with_group["group_by"] = json!("model");
        assert!(serde_json::from_value::<AlertCondition>(with_group).is_err());

        let mut two_hours = value;
        two_hours["window"] = json!("2h");
        assert!(serde_json::from_value::<AlertCondition>(two_hours).is_err());
    }

    #[test]
    fn window_interval() {
        assert_eq!(AlertMetric::LatencyP95.as_str(), "latency_p95");
        assert_eq!(AlertWindow::M15.interval_ns(), 60_000_000_000);
        assert_eq!(AlertWindow::H1.interval_ns(), 300_000_000_000);
        assert_eq!(AlertWindow::H24.ns(), 86_400_000_000_000);
        assert_eq!(AlertWindow::M5.as_str(), "5m");
    }

    #[test]
    fn comparator_breaches() {
        assert!(!Comparator::Gt.breaches(1.0, 1.0));
        assert!(Comparator::Gt.breaches(1.1, 1.0));
        assert!(Comparator::Gte.breaches(1.0, 1.0));
        assert!(Comparator::Lt.breaches(0.5, 1.0));
        assert!(!Comparator::Lt.breaches(1.0, 1.0));
        assert!(Comparator::Lte.breaches(1.0, 1.0));
        assert!(!Comparator::Lte.breaches(1.1, 1.0));
    }
}
