//! Per-project retention (spec 03 § 8, D-M-10, D-M-15).

use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate};
use llmobs_core::ids::ProjectId;

use crate::http::AppState;
use crate::meta::MetaError;

/// Time between two runs.
pub(crate) const EVERY: Duration = Duration::from_secs(3600);
/// Delay of the first run after startup.
pub(crate) const FIRST_RUN: Duration = Duration::from_secs(60);

/// The first UTC day to keep: the day of `now_ns`, minus `days`.
pub(crate) fn keep_from(now_ns: i64, days: u32) -> NaiveDate {
    let today = DateTime::from_timestamp_nanos(now_ns).date_naive();
    // i64 ns covers 1677–2262 and days ≤ 36_500, so this cannot leave chrono's range;
    // `NaiveDate::MIN` only keeps the function total.
    today
        .checked_sub_days(Days::new(u64::from(days)))
        .unwrap_or(NaiveDate::MIN)
}

/// Effective retention: the project's own and the org's cap, whichever is shorter;
/// the one that is set when only one is; none when neither is (D-M-15).
pub(crate) fn effective_days(project: Option<u32>, cap: Option<u32>) -> Option<u32> {
    match (project, cap) {
        (Some(days), Some(cap)) => Some(days.min(cap)),
        (Some(days), None) | (None, Some(days)) => Some(days),
        (None, None) => None,
    }
}

/// Totals of one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RetentionRun {
    /// Projects whose call succeeded.
    pub(crate) projects: u64,
    /// Projects whose call failed (logged).
    pub(crate) failures: u64,
    /// Files obsoleted, all projects.
    pub(crate) files: u64,
}

/// Applies the effective retention of every active project of an active org.
/// A storage error on one project is logged and counted; the others still run.
///
/// # Errors
///
/// [`MetaError`] when the projects cannot be listed.
pub(crate) async fn retention_once(state: &AppState) -> Result<RetentionRun, MetaError> {
    let now = state.clock.now_ns();
    let mut run = RetentionRun::default();
    for (_org, project, own) in
        crate::meta::projects::projects_for_retention(state.meta.reader()).await?
    {
        // D175, D-M-15: track P replaces `None` with the org's cap
        // (`state.entitlements.of(_org).retention_cap_days()`: data-access window + 30 days).
        let cap: Option<u32> = None;
        let Some(days) = effective_days(own, cap) else {
            continue;
        };
        // A project dropped by storage is a no-op there (`committer::apply_retention` finds it
        // in `projects`), so a drop that races this list writes nothing.
        match state
            .storage
            .apply_retention(project, keep_from(now, days))
            .await
        {
            Ok(report) => {
                run.projects += 1;
                run.files += report.files;
            }
            Err(error) => {
                run.failures += 1;
                tracing::warn!(project_id = %project, error = %error, "retention failed");
            }
        }
    }
    Ok(run)
}

/// Applies `days` to one project now (a `PATCH`). Errors are logged; the hourly run retries.
pub(crate) async fn apply_now(state: &AppState, project: ProjectId, days: u32) {
    let keep = keep_from(state.clock.now_ns(), days);
    if let Err(error) = state.storage.apply_retention(project, keep).await {
        tracing::warn!(project_id = %project, error = %error, "retention failed");
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::span::Span;

    use super::*;
    use crate::meta::projects::ProjectRow;
    use crate::test_support::{Seeded, TestApp, dataset_span};

    /// 2026-01-01T00:00:00Z, the manual clock's start.
    const NOW: i64 = 1_767_225_600_000_000_000;
    const DAY: i64 = 86_400 * 1_000_000_000;
    const HOUR: i64 = 3_600 * 1_000_000_000;

    #[test]
    fn keep_from_counts_utc_days() {
        assert_eq!(
            keep_from(NOW, 30),
            NaiveDate::from_ymd_opt(2025, 12, 2).unwrap()
        );
        assert_eq!(
            keep_from(NOW + DAY - 1, 1),
            NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()
        );
        assert_eq!(
            keep_from(NOW, 36_500),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() - Days::new(36_500)
        );
        let _ = keep_from(i64::MIN, 1);
    }

    #[test]
    fn effective_days_takes_the_shorter() {
        assert_eq!(effective_days(Some(30), Some(7)), Some(7));
        assert_eq!(effective_days(Some(7), Some(30)), Some(7));
        assert_eq!(effective_days(Some(7), None), Some(7));
        assert_eq!(effective_days(None, Some(30)), Some(30));
        assert_eq!(effective_days(None, None), None);
    }

    /// A trace id of the tests: `a3`, then `n` in 30 hex digits.
    fn trace_id(n: u32) -> String {
        format!("a3{n:030x}")
    }

    /// Commits one span of `trace` to `project`, with `start_time` as given.
    async fn commit_at(app: &TestApp, project: &ProjectRow, trace: &str, start_time: i64) {
        let mut row: Span = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().unwrap();
        row.span_id = "b3000000000000aa".parse::<SpanId>().unwrap();
        row.parent_span_id = None;
        row.start_time = start_time;
        row.end_time = start_time + 1_000;
        row.ingest_time = app.state.clock.now_ns();
        app.commit_spans(project, vec![row]).await;
    }

    /// `GET /api/v1/projects/{project}/traces/{trace}` with the seeded cookie; returns the status.
    async fn trace_status(
        app: &TestApp,
        seeded: &Seeded,
        project: ProjectId,
        trace: &str,
    ) -> StatusCode {
        app.request(
            Method::GET,
            &format!("/api/v1/projects/{project}/traces/{trace}"),
        )
        .header("cookie", &seeded.cookie)
        .send()
        .await
        .status
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hourly_run_drops_old_days() {
        let app = TestApp::new().await;
        app.state.clock.set(NOW);
        let seeded = app.seed().await;
        let (short, _) = app.add_project(&seeded, "short", Some(1)).await;
        commit_at(&app, &short, &trace_id(1), NOW - 3 * DAY + 12 * HOUR).await;
        commit_at(&app, &short, &trace_id(2), NOW - 12 * HOUR).await;
        let plain = seeded.project.clone();
        commit_at(&app, &plain, &trace_id(3), NOW - 3 * DAY + 12 * HOUR).await;
        commit_at(&app, &plain, &trace_id(4), NOW - 12 * HOUR).await;

        let run = retention_once(&app.state).await.unwrap();
        assert_eq!(run.projects, 1);
        assert_eq!(run.failures, 0);

        assert_eq!(
            trace_status(&app, &seeded, short.id, &trace_id(1)).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            trace_status(&app, &seeded, short.id, &trace_id(2)).await,
            StatusCode::OK
        );
        assert_eq!(
            trace_status(&app, &seeded, plain.id, &trace_id(3)).await,
            StatusCode::OK
        );
        assert_eq!(
            trace_status(&app, &seeded, plain.id, &trace_id(4)).await,
            StatusCode::OK
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn inactive_projects_are_skipped() {
        let app = TestApp::new().await;
        app.state.clock.set(NOW);
        let seeded = app.seed().await;
        let (deleting, _) = app.add_project(&seeded, "p-del", Some(1)).await;
        let _ = app.add_project(&seeded, "p-ok", Some(1)).await;
        let deleting_id = deleting.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE projects SET state = 'deleting' WHERE id = ?")
                        .bind(deleting_id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();

        let run = retention_once(&app.state).await.unwrap();
        assert_eq!(run.projects, 1);

        let org_id = seeded.org_id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE orgs SET state = 'deleting' WHERE id = ?")
                        .bind(org_id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let run = retention_once(&app.state).await.unwrap();
        assert_eq!(run.projects, 0);
    }
}
