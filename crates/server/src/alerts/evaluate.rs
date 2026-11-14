//! One evaluation pass over the due rules, and the rule state machine (spec 07, D-K-3, D-K-4).

use std::time::Duration;

use futures::StreamExt;
use llmobs_core::ids::ProjectId;
use llmobs_storage::{AlertValue, AlertValueRequest, RunError, TimeWindow, alert_value};
use uuid::Uuid;

use crate::alerts::condition::measure;
use crate::api::alerts::{AlertCondition, AlertEventKind, AlertState};
use crate::http::AppState;
use crate::http::error::ApiError;
use crate::meta::MetaError;
use crate::meta::alerts::{
    Evaluation, EventDetails, Recorded, RuleRow, due_rules, record_evaluation,
};

/// Rules evaluated at once (D-K-3).
pub(crate) const CONCURRENCY: usize = 4;
/// Time allowed for one rule's query (D-K-3).
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// A rule is due this much early, so a 60 s tick never skips a 60 s interval (D-K-3).
pub(crate) const DUE_SLACK_NS: i64 = 5_000_000_000;
/// Clear or insufficient evaluations in a row that resolve a firing rule (D-K-4).
pub(crate) const CLEARS_TO_RESOLVE: i64 = 2;

/// The state after one successful evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) state: AlertState,
    pub(crate) clear_streak: i64,
    /// The event to record, if any.
    pub(crate) event: Option<AlertEventKind>,
    /// This evaluation fired the rule: `last_fired_at = now`.
    pub(crate) fired: bool,
    /// A `fired` or `reminder` event: `last_notified_at = now`.
    pub(crate) notified: bool,
}

/// How one measurement reads against the condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Fewer samples than `min_count`.
    Insufficient,
    /// Enough samples, a value, and the comparator holds.
    Breaching,
    /// Anything else.
    Clear,
}

/// Classifies a measurement against `condition`.
pub(crate) fn reading(condition: &AlertCondition, count: i64, value: Option<f64>) -> Reading {
    if condition.min_count.is_some_and(|min| count < min) {
        Reading::Insufficient
    } else if value.is_some_and(|value| condition.comparator.breaches(value, condition.threshold)) {
        Reading::Breaching
    } else {
        Reading::Clear
    }
}

/// The next state of `rule` after `reading` at `now` (D-K-4, the table in spec 07).
pub(crate) fn step(rule: &RuleRow, reading: Reading, now: i64) -> Step {
    match (rule.state, reading) {
        (AlertState::Ok | AlertState::NoData, Reading::Breaching) => Step {
            state: AlertState::Firing,
            clear_streak: 0,
            event: Some(AlertEventKind::Fired),
            fired: true,
            notified: true,
        },
        (AlertState::Ok | AlertState::NoData, Reading::Clear) => quiet(AlertState::Ok, 0, None),
        (AlertState::Ok | AlertState::NoData, Reading::Insufficient) => {
            quiet(AlertState::NoData, 0, None)
        }
        (AlertState::Firing, Reading::Breaching) => {
            if reminder_due(rule, now) {
                Step {
                    state: AlertState::Firing,
                    clear_streak: 0,
                    event: Some(AlertEventKind::Reminder),
                    fired: false,
                    notified: true,
                }
            } else {
                quiet(AlertState::Firing, 0, None)
            }
        }
        (AlertState::Firing, Reading::Clear | Reading::Insufficient) => {
            let streak = rule.clear_streak.saturating_add(1);
            if streak < CLEARS_TO_RESOLVE {
                quiet(AlertState::Firing, streak, None)
            } else {
                let state = if reading == Reading::Insufficient {
                    AlertState::NoData
                } else {
                    AlertState::Ok
                };
                quiet(state, 0, Some(AlertEventKind::Resolved))
            }
        }
    }
}

/// A step that neither fires nor notifies.
fn quiet(state: AlertState, clear_streak: i64, event: Option<AlertEventKind>) -> Step {
    Step {
        state,
        clear_streak,
        event,
        fired: false,
        notified: false,
    }
}

/// A firing rule with reminders on sends one once its cooldown has passed since the last
/// notification.
fn reminder_due(rule: &RuleRow, now: i64) -> bool {
    rule.notify_while_firing
        && rule.last_notified_at.is_none_or(|at| {
            now.saturating_sub(at) >= rule.cooldown_s.saturating_mul(1_000_000_000)
        })
}

/// What one pass did. Logged by the loop (K9) and asserted by tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EvaluateReport {
    pub(crate) evaluated: usize,
    pub(crate) failed: usize,
    pub(crate) events: usize,
    pub(crate) stale: usize,
}

/// Evaluates every due rule once at `now`, at most [`CONCURRENCY`] at a time.
///
/// # Errors
///
/// Returns [`MetaError`] only when the due rules cannot be read. A failed query or a failed
/// write is logged and counted in the report.
pub(crate) async fn evaluate_once(state: &AppState, now: i64) -> Result<EvaluateReport, MetaError> {
    let due = due_rules(state.meta.reader(), now, DUE_SLACK_NS).await?;
    let results = futures::stream::iter(due)
        .map(|rule| Box::pin(evaluate_rule(state, rule, now)))
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    let mut report = EvaluateReport::default();
    for (rule_id, query_failed, written) in results {
        report.evaluated += 1;
        let mut failed = query_failed;
        match written {
            Ok(Recorded::Event { .. }) => report.events += 1,
            Ok(Recorded::Stale) => report.stale += 1,
            Ok(Recorded::Written) => {}
            Err(error) => {
                tracing::error!(rule_id = %rule_id, error = %error, "alert evaluation write failed");
                failed = true;
            }
        }
        if failed {
            report.failed += 1;
        }
    }
    Ok(report)
}

/// Evaluates one rule. Returns its id, whether its query failed, and the write's result.
async fn evaluate_rule(
    state: &AppState,
    rule: RuleRow,
    now: i64,
) -> (Uuid, bool, Result<Recorded, MetaError>) {
    let rule_id = rule.id;
    let project = rule.project_id;
    let expected_updated_at = rule.updated_at;
    let window_from = now.saturating_sub(rule.condition.window.ns());
    let mut event_kind = None;
    let evaluation = match measure_rule(state, project, &rule.condition, window_from, now).await {
        Err(reason) => {
            tracing::warn!(rule_id = %rule_id, reason, "alert evaluation failed");
            Evaluation::Failed {
                message: reason.to_owned(),
            }
        }
        Ok(measured) => {
            let read = reading(&rule.condition, measured.count, measured.value);
            let next = step(&rule, read, now);
            event_kind = next.event;
            Evaluation::Measured {
                step: next,
                count: measured.count,
                value: measured.value,
                threshold: rule.condition.threshold,
                window_from,
                window_to: now,
                details: EventDetails {
                    rule_name: rule.name,
                    metric: rule.condition.metric,
                    comparator: rule.condition.comparator,
                    window: rule.condition.window,
                    filter: rule.condition.filter,
                },
            }
        }
    };
    let query_failed = matches!(evaluation, Evaluation::Failed { .. });
    let written = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                record_evaluation(conn, rule_id, expected_updated_at, evaluation, now).await
            })
        })
        .await;
    if let Ok(Recorded::Event {
        event_id,
        deliveries,
    }) = &written
    {
        tracing::info!(
            rule_id = %rule_id,
            event_id = %event_id,
            kind = event_kind.map_or("", AlertEventKind::as_str),
            deliveries,
            "alert event"
        );
    }
    (rule_id, query_failed, written)
}

/// Measures the rule's condition over `[from, to)`. The error is a fixed reason, never a
/// filter or a query message.
async fn measure_rule(
    state: &AppState,
    project: ProjectId,
    condition: &AlertCondition,
    from: i64,
    to: i64,
) -> Result<AlertValue, &'static str> {
    let window = TimeWindow::new(from, to).map_err(|_| "invalid window")?;
    let ctx = crate::routes::read::open_query_on(&state.storage, project, None, None)
        .await
        .map_err(|error| match error {
            ApiError::Unavailable { .. } => "storage is busy",
            _ => "query failed",
        })?;
    let request = AlertValueRequest {
        window,
        filter: condition.filter.as_deref(),
        measure: measure(condition),
        timeout: QUERY_TIMEOUT,
    };
    Box::pin(alert_value(&ctx, request))
        .await
        .map_err(|error| match error {
            RunError::Timeout => "query timed out",
            RunError::TooLarge => "query needed more memory than allowed",
            RunError::Invalid(_) => "the filter is no longer valid",
            _ => "query failed",
        })
}

#[cfg(test)]
mod tests {
    use llmobs_core::enums::SpanStatus;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::span::Span;

    use super::*;
    use crate::api::alerts::{AlertMetric, AlertWindow, ChannelKind, Comparator, DeliveryStatus};
    use crate::meta::alerts::{
        NewChannel, NewRule, RulePatch, deliveries_for_events, insert_channel, insert_rule,
        list_events, rule_by_id, update_rule,
    };
    use crate::test_support::{TestApp, assert_logs_clean, dataset_span};

    const MINUTE: Duration = Duration::from_secs(60);
    const MINUTE_NS: i64 = 60_000_000_000;

    fn rule(state: AlertState, streak: i64) -> RuleRow {
        RuleRow {
            id: Uuid::now_v7(),
            project_id: ProjectId::new_v7(),
            name: "r".to_owned(),
            condition: error_rate(AlertWindow::M5, 0.1, Some(10), None),
            channel_ids: Vec::new(),
            enabled: true,
            cooldown_s: 3600,
            notify_while_firing: false,
            state,
            clear_streak: streak,
            last_value: None,
            last_count: None,
            last_evaluated_at: None,
            last_fired_at: None,
            last_notified_at: None,
            last_error: None,
            created_by: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn error_rate(
        window: AlertWindow,
        threshold: f64,
        min_count: Option<i64>,
        filter: Option<&str>,
    ) -> AlertCondition {
        AlertCondition {
            metric: AlertMetric::ErrorRate,
            window,
            filter: filter.map(str::to_owned),
            comparator: Comparator::Gt,
            threshold,
            min_count,
            target: None,
            score_name: None,
            aggregation: None,
        }
    }

    #[test]
    fn reading_classifies() {
        let condition = error_rate(AlertWindow::M5, 0.1, Some(10), None);
        assert_eq!(reading(&condition, 9, Some(0.2)), Reading::Insufficient);
        assert_eq!(reading(&condition, 10, Some(0.2)), Reading::Breaching);
        assert_eq!(reading(&condition, 10, Some(0.1)), Reading::Clear);
        assert_eq!(reading(&condition, 10, None), Reading::Clear);

        let cost = AlertCondition {
            metric: AlertMetric::Cost,
            min_count: None,
            comparator: Comparator::Gte,
            threshold: 0.0,
            ..error_rate(AlertWindow::M5, 0.0, None, None)
        };
        assert_eq!(reading(&cost, 0, Some(0.0)), Reading::Breaching);
    }

    #[test]
    fn step_table() {
        let now = 1_000 * MINUTE_NS;
        let fired = Step {
            state: AlertState::Firing,
            clear_streak: 0,
            event: Some(AlertEventKind::Fired),
            fired: true,
            notified: true,
        };
        for state in [AlertState::Ok, AlertState::NoData] {
            assert_eq!(
                step(&rule(state, 0), Reading::Breaching, now),
                fired,
                "{state:?}"
            );
            assert_eq!(
                step(&rule(state, 0), Reading::Clear, now),
                quiet(AlertState::Ok, 0, None)
            );
            assert_eq!(
                step(&rule(state, 0), Reading::Insufficient, now),
                quiet(AlertState::NoData, 0, None)
            );
        }

        let mut reminding = rule(AlertState::Firing, 0);
        reminding.notify_while_firing = true;
        reminding.last_notified_at = None;
        assert_eq!(
            step(&reminding, Reading::Breaching, now),
            Step {
                state: AlertState::Firing,
                clear_streak: 0,
                event: Some(AlertEventKind::Reminder),
                fired: false,
                notified: true,
            }
        );
        assert_eq!(
            step(&rule(AlertState::Firing, 0), Reading::Breaching, now),
            quiet(AlertState::Firing, 0, None)
        );
        assert_eq!(
            step(&rule(AlertState::Firing, 0), Reading::Clear, now),
            quiet(AlertState::Firing, 1, None)
        );
        assert_eq!(
            step(&rule(AlertState::Firing, 0), Reading::Insufficient, now),
            quiet(AlertState::Firing, 1, None)
        );
        assert_eq!(
            step(&rule(AlertState::Firing, 1), Reading::Clear, now),
            quiet(AlertState::Ok, 0, Some(AlertEventKind::Resolved))
        );
        assert_eq!(
            step(&rule(AlertState::Firing, 1), Reading::Insufficient, now),
            quiet(AlertState::NoData, 0, Some(AlertEventKind::Resolved))
        );
    }

    #[test]
    fn reminders_respect_the_cooldown() {
        let now = 10_000 * 1_000_000_000;
        let mut firing = rule(AlertState::Firing, 0);
        firing.notify_while_firing = true;
        firing.last_notified_at = Some(now - 3599 * 1_000_000_000);
        assert_eq!(
            step(&firing, Reading::Breaching, now).event,
            None,
            "inside the cooldown"
        );

        firing.last_notified_at = Some(now - 3600 * 1_000_000_000);
        let reminder = step(&firing, Reading::Breaching, now);
        assert_eq!(reminder.event, Some(AlertEventKind::Reminder));
        assert!(reminder.notified);

        firing.notify_while_firing = false;
        assert_eq!(step(&firing, Reading::Breaching, now).event, None);
    }

    fn root_span(app: &TestApp, index: u8, ago: Duration, error: bool) -> Span {
        let mut span = dataset_span(0);
        let start = app.state.clock.now_ns() - i64::try_from(ago.as_nanos()).expect("small");
        span.trace_id = TraceId::from_bytes([index; 16]).expect("trace id");
        span.span_id = SpanId::from_bytes([index; 8]).expect("span id");
        span.parent_span_id = None;
        span.start_time = start;
        span.end_time = start + 1_000_000;
        span.status = if error {
            SpanStatus::Error
        } else {
            SpanStatus::Ok
        };
        span.environment = None;
        span.metadata.clear();
        span
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("a value");
        assert!(
            (actual - expected).abs() < 1e-9,
            "{actual} is not {expected}"
        );
    }

    async fn add_channel(app: &TestApp, project: ProjectId, name: &str) -> Uuid {
        let channel = NewChannel {
            id: Uuid::now_v7(),
            project_id: project,
            kind: ChannelKind::Webhook,
            name: name.to_owned(),
            config_enc: vec![1, 2, 3],
            target_hint: "hooks.example.com".to_owned(),
        };
        let id = channel.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { insert_channel(conn, channel, 1).await.map(|_| ()) })
            })
            .await
            .expect("channel");
        id
    }

    async fn add_rule(
        app: &TestApp,
        project: ProjectId,
        name: &str,
        condition: AlertCondition,
        channel_ids: Vec<Uuid>,
    ) -> RuleRow {
        let rule = NewRule {
            project_id: project,
            name: name.to_owned(),
            condition,
            channel_ids,
            enabled: true,
            cooldown_s: 3600,
            notify_while_firing: false,
            created_by: None,
        };
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| Box::pin(async move { insert_rule(conn, rule, now).await }))
            .await
            .expect("rule")
    }

    async fn read_rule(app: &TestApp, project: ProjectId, id: Uuid) -> RuleRow {
        rule_by_id(app.state.meta.reader(), project, id)
            .await
            .expect("query")
            .expect("rule")
    }

    #[tokio::test]
    async fn fires_then_resolves_with_the_manual_clock() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let channel = add_channel(&app, project, "ops").await;
        let rule = add_rule(
            &app,
            project,
            "errors",
            error_rate(AlertWindow::M5, 0.4, Some(2), None),
            vec![channel],
        )
        .await;
        app.commit_spans(
            &seeded.project,
            vec![
                root_span(&app, 1, MINUTE, true),
                root_span(&app, 2, MINUTE, true),
                root_span(&app, 3, MINUTE, false),
            ],
        )
        .await;

        let now = app.state.clock.now_ns();
        let report = evaluate_once(&app.state, now).await.unwrap();
        assert_eq!(
            report,
            EvaluateReport {
                evaluated: 1,
                events: 1,
                ..EvaluateReport::default()
            }
        );
        let row = read_rule(&app, project, rule.id).await;
        assert_eq!(row.state, AlertState::Firing);
        assert_close(row.last_value, 2.0 / 3.0);
        assert_eq!(row.last_count, Some(3));
        let events = list_events(app.state.meta.reader(), rule.id, None, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        let fired = &events[0];
        assert_eq!(fired.kind, AlertEventKind::Fired);
        assert_close(fired.value, 2.0 / 3.0);
        assert_eq!(fired.count, Some(3));
        assert_eq!(fired.threshold, 0.4);
        assert_eq!(fired.window_to, now);
        assert_eq!(fired.window_from, now - 5 * MINUTE_NS);
        let deliveries = deliveries_for_events(app.state.meta.reader(), &[fired.id])
            .await
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, DeliveryStatus::Pending);

        // Not due yet: 30 s is under the 55 s due point.
        app.state.clock.advance(Duration::from_secs(30));
        let now = app.state.clock.now_ns();
        assert_eq!(
            evaluate_once(&app.state, now).await.unwrap(),
            EvaluateReport::default()
        );

        // Due again and still breaching: no new event (no reminders).
        app.state.clock.advance(Duration::from_secs(30));
        let now = app.state.clock.now_ns();
        assert_eq!(
            evaluate_once(&app.state, now).await.unwrap(),
            EvaluateReport {
                evaluated: 1,
                ..EvaluateReport::default()
            }
        );
        let events = list_events(app.state.meta.reader(), rule.id, None, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);

        // The traces leave the window: insufficient, first clear evaluation.
        app.state.clock.advance(Duration::from_secs(6 * 60));
        let now = app.state.clock.now_ns();
        assert_eq!(
            evaluate_once(&app.state, now).await.unwrap(),
            EvaluateReport {
                evaluated: 1,
                ..EvaluateReport::default()
            }
        );
        let row = read_rule(&app, project, rule.id).await;
        assert_eq!(row.state, AlertState::Firing);
        assert_eq!(row.clear_streak, 1);

        // The second one resolves the rule.
        app.state.clock.advance(Duration::from_secs(60));
        let now = app.state.clock.now_ns();
        assert_eq!(
            evaluate_once(&app.state, now).await.unwrap(),
            EvaluateReport {
                evaluated: 1,
                events: 1,
                ..EvaluateReport::default()
            }
        );
        let row = read_rule(&app, project, rule.id).await;
        assert_eq!(row.state, AlertState::NoData);
        assert_eq!(row.clear_streak, 0);
        let events = list_events(app.state.meta.reader(), rule.id, None, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        let resolved = &events[0];
        assert_eq!(resolved.kind, AlertEventKind::Resolved);
        assert_eq!(resolved.value, None);
        assert_eq!(resolved.count, Some(0));
        let ids: Vec<Uuid> = events.iter().map(|event| event.id).collect();
        let deliveries = deliveries_for_events(app.state.meta.reader(), &ids)
            .await
            .unwrap();
        assert_eq!(deliveries.len(), 2);
        assert!(
            deliveries
                .iter()
                .all(|delivery| delivery.status == DeliveryStatus::Pending)
        );
    }

    #[tokio::test]
    async fn a_failed_query_records_the_error() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let rule = add_rule(
            &app,
            project,
            "broken",
            error_rate(AlertWindow::M5, 0.4, Some(2), Some("nonsense_column = 1")),
            Vec::new(),
        )
        .await;
        let now = app.state.clock.now_ns();

        let report = evaluate_once(&app.state, now).await.unwrap();
        assert_eq!(
            report,
            EvaluateReport {
                evaluated: 1,
                failed: 1,
                ..EvaluateReport::default()
            }
        );
        let row = read_rule(&app, project, rule.id).await;
        assert_eq!(row.state, AlertState::Ok);
        assert_eq!(
            row.last_error.as_deref(),
            Some("the filter is no longer valid")
        );
        assert_eq!(row.last_evaluated_at, Some(now));
        let events = list_events(app.state.meta.reader(), rule.id, None, 10)
            .await
            .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn an_edit_during_evaluation_is_not_overwritten() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let rule = add_rule(
            &app,
            project,
            "edited",
            error_rate(AlertWindow::M5, 0.4, Some(2), None),
            Vec::new(),
        )
        .await;
        let now = app.state.clock.now_ns();
        let read = read_rule(&app, project, rule.id).await;
        let id = rule.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    update_rule(
                        conn,
                        project,
                        id,
                        RulePatch {
                            name: Some("renamed".to_owned()),
                            ..RulePatch::default()
                        },
                        now + 1,
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await
            .unwrap();

        let recorded = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    record_evaluation(
                        conn,
                        id,
                        read.updated_at,
                        Evaluation::Failed {
                            message: "x".to_owned(),
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(recorded, Recorded::Stale);
        let row = read_rule(&app, project, id).await;
        assert_eq!(row.name, "renamed");
        assert_eq!(row.last_error, None);
    }

    #[tokio::test]
    async fn due_rules_order_and_stale_count() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let now = app.state.clock.now_ns();
        let broken = add_rule(
            &app,
            project,
            "broken",
            error_rate(AlertWindow::M5, 0.4, Some(2), Some("nonsense_column = 1")),
            Vec::new(),
        )
        .await;
        let quiet_rule = add_rule(
            &app,
            project,
            "quiet",
            error_rate(AlertWindow::M5, 0.4, Some(2), None),
            Vec::new(),
        )
        .await;
        let old = add_rule(
            &app,
            project,
            "old",
            error_rate(AlertWindow::M5, 0.4, Some(2), None),
            Vec::new(),
        )
        .await;
        set_last_evaluated(&app, old.id, now - 3600 * 1_000_000_000).await;

        // Never evaluated first, by id; then the rule evaluated long ago.
        let due: Vec<Uuid> = due_rules(app.state.meta.reader(), now, DUE_SLACK_NS)
            .await
            .unwrap()
            .iter()
            .map(|rule| rule.id)
            .collect();
        let mut never = [broken.id, quiet_rule.id];
        never.sort();
        assert_eq!(due, vec![never[0], never[1], old.id]);

        // The old rule was evaluated 30 s ago, so it is not due; the other two are.
        set_last_evaluated(&app, old.id, now - 30 * 1_000_000_000).await;
        let report = evaluate_once(&app.state, now).await.unwrap();
        assert_eq!(
            report,
            EvaluateReport {
                evaluated: 2,
                failed: 1,
                events: 0,
                stale: 0,
            }
        );
    }

    /// Sets `id`'s `last_evaluated_at` by SQL.
    async fn set_last_evaluated(app: &TestApp, id: Uuid, at: i64) {
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE alert_rules SET last_evaluated_at = ? WHERE id = ?")
                        .bind(at)
                        .bind(id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn evaluation_logs_hold_no_filter() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let rule = add_rule(
            &app,
            project,
            "leaky",
            error_rate(
                AlertWindow::M5,
                0.4,
                Some(2),
                Some("missing_column = 'secret-env'"),
            ),
            Vec::new(),
        )
        .await;
        let now = app.state.clock.now_ns();

        let report = assert_logs_clean(
            &["secret-env", "missing_column"],
            evaluate_once(&app.state, now),
        )
        .await
        .unwrap();
        assert_eq!(report.failed, 1);
        let row = read_rule(&app, project, rule.id).await;
        assert_eq!(
            row.last_error.as_deref(),
            Some("the filter is no longer valid")
        );
    }
}
