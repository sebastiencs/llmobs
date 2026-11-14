//! Usage counters (spec 04 § Metering, D-G-11, D-G-30).
//!
//! [`Usage::add`] sums accepted spans, request bytes and appended scores per org, project
//! and UTC day. [`Usage::flush`] upserts the sums into `usage_daily` and clears them.
//! `llmobs serve` flushes every 10 s and once at shutdown.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate};
use llmobs_core::ids::{OrgId, ProjectId};

use crate::meta::MetaDb;
use crate::meta::MetaError;

/// One `usage_daily` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UsageKey {
    org_id: OrgId,
    project_id: ProjectId,
    day: NaiveDate,
}

/// The counts of one `usage_daily` row.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct UsageCounts {
    spans: u64,
    bytes: u64,
    scores: u64,
    judge_input_tokens: u64,
    judge_output_tokens: u64,
    judge_cost_usd: f64,
}

impl UsageCounts {
    fn merge(&mut self, other: Self) {
        self.spans = self.spans.saturating_add(other.spans);
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.scores = self.scores.saturating_add(other.scores);
        self.judge_input_tokens = self
            .judge_input_tokens
            .saturating_add(other.judge_input_tokens);
        self.judge_output_tokens = self
            .judge_output_tokens
            .saturating_add(other.judge_output_tokens);
        self.judge_cost_usd += other.judge_cost_usd;
    }
}

/// In-memory usage counters, upserted into `usage_daily` (D-G-11).
#[derive(Debug, Default)]
pub(crate) struct Usage {
    counts: Mutex<HashMap<UsageKey, UsageCounts>>,
}

impl Usage {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<UsageKey, UsageCounts>> {
        self.counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Adds counts to the day of `now_ns` (UTC). All-zero counts change nothing.
    pub(crate) fn add(
        &self,
        org_id: OrgId,
        project_id: ProjectId,
        now_ns: i64,
        spans: u64,
        bytes: u64,
        scores: u64,
    ) {
        if spans == 0 && bytes == 0 && scores == 0 {
            return;
        }
        self.merge_into(
            org_id,
            project_id,
            now_ns,
            UsageCounts {
                spans,
                bytes,
                scores,
                ..UsageCounts::default()
            },
        );
    }

    /// Adds one judge call's tokens and cost to the day of `now_ns` (UTC) (D-I-31). A zero call
    /// changes nothing. A NaN, infinite or negative cost counts as 0, so it cannot break the
    /// `judge_cost_usd` column and every later flush.
    pub(crate) fn add_judge(
        &self,
        org_id: OrgId,
        project_id: ProjectId,
        now_ns: i64,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) {
        let cost_usd = if cost_usd.is_finite() && cost_usd > 0.0 {
            cost_usd
        } else {
            0.0
        };
        if input_tokens == 0 && output_tokens == 0 && cost_usd == 0.0 {
            return;
        }
        self.merge_into(
            org_id,
            project_id,
            now_ns,
            UsageCounts {
                judge_input_tokens: input_tokens,
                judge_output_tokens: output_tokens,
                judge_cost_usd: cost_usd,
                ..UsageCounts::default()
            },
        );
    }

    fn merge_into(&self, org_id: OrgId, project_id: ProjectId, now_ns: i64, counts: UsageCounts) {
        let key = UsageKey {
            org_id,
            project_id,
            day: DateTime::from_timestamp_nanos(now_ns).date_naive(),
        };
        self.lock().entry(key).or_default().merge(counts);
    }

    /// Upserts the counted sums into `usage_daily` and clears them, in one transaction.
    ///
    /// A failed write adds the taken sums back, so the next flush writes them.
    ///
    /// # Errors
    ///
    /// Returns [`MetaError`] when the write fails.
    pub(crate) async fn flush(&self, meta: &MetaDb) -> Result<(), MetaError> {
        let rows = {
            let mut counts = self.lock();
            if counts.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *counts).into_iter().collect::<Vec<_>>()
        };
        let saved = rows.clone();
        let written = meta
            .write(move |conn| {
                Box::pin(async move {
                    for (key, counts) in rows {
                        sqlx::query(
                            "INSERT INTO usage_daily (org_id, project_id, day, spans, bytes, scores, \
                             judge_input_tokens, judge_output_tokens, judge_cost_usd) \
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                             ON CONFLICT (org_id, project_id, day) DO UPDATE SET \
                             spans = spans + excluded.spans, \
                             bytes = bytes + excluded.bytes, \
                             scores = scores + excluded.scores, \
                             judge_input_tokens = judge_input_tokens + excluded.judge_input_tokens, \
                             judge_output_tokens = judge_output_tokens + excluded.judge_output_tokens, \
                             judge_cost_usd = judge_cost_usd + excluded.judge_cost_usd",
                        )
                        .bind(key.org_id.to_string())
                        .bind(key.project_id.to_string())
                        .bind(key.day.format("%Y-%m-%d").to_string())
                        .bind(i64::try_from(counts.spans).unwrap_or(i64::MAX))
                        .bind(i64::try_from(counts.bytes).unwrap_or(i64::MAX))
                        .bind(i64::try_from(counts.scores).unwrap_or(i64::MAX))
                        .bind(i64::try_from(counts.judge_input_tokens).unwrap_or(i64::MAX))
                        .bind(i64::try_from(counts.judge_output_tokens).unwrap_or(i64::MAX))
                        .bind(counts.judge_cost_usd)
                        .execute(&mut *conn)
                        .await?;
                    }
                    Ok(())
                })
            })
            .await;
        if let Err(error) = written {
            let mut counts = self.lock();
            for (key, taken) in saved {
                counts.entry(key).or_default().merge(taken);
            }
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::test_db;

    /// 2026-01-01T12:00:00Z.
    const DAY_ONE: i64 = 1_767_268_800_000_000_000;
    /// 2026-01-02T12:00:00Z.
    const DAY_TWO: i64 = DAY_ONE + 86_400_000_000_000;

    /// `(day, spans, bytes, scores)` of every `usage_daily` row of `project`, by day.
    async fn stored(meta: &MetaDb, project: ProjectId) -> Vec<(String, i64, i64, i64)> {
        sqlx::query_as(
            "SELECT day, spans, bytes, scores FROM usage_daily \
             WHERE project_id = ? ORDER BY day",
        )
        .bind(project.to_string())
        .fetch_all(meta.reader())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn flush_sums_the_counts_of_each_day() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add(org, project, DAY_ONE, 2, 100, 0);
        usage.add(org, project, DAY_ONE, 1, 50, 3);
        usage.add(org, project, DAY_TWO, 4, 7, 0);
        usage.flush(&meta).await.unwrap();
        assert_eq!(
            stored(&meta, project).await,
            vec![
                ("2026-01-01".to_owned(), 3, 150, 3),
                ("2026-01-02".to_owned(), 4, 7, 0),
            ]
        );
    }

    #[tokio::test]
    async fn a_second_flush_adds_to_the_stored_row() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add(org, project, DAY_ONE, 3, 0, 0);
        usage.flush(&meta).await.unwrap();
        usage.add(org, project, DAY_ONE, 2, 0, 0);
        usage.flush(&meta).await.unwrap();
        assert_eq!(
            stored(&meta, project).await,
            vec![("2026-01-01".to_owned(), 5, 0, 0)]
        );
    }

    #[tokio::test]
    async fn zero_counts_write_nothing() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add(org, project, DAY_ONE, 0, 0, 0);
        usage.flush(&meta).await.unwrap();
        assert!(stored(&meta, project).await.is_empty());
    }

    /// `(judge_input_tokens, judge_output_tokens, judge_cost_usd)` of `project`'s rows, by day.
    async fn stored_judge(meta: &MetaDb, project: ProjectId) -> Vec<(i64, i64, f64)> {
        sqlx::query_as(
            "SELECT judge_input_tokens, judge_output_tokens, judge_cost_usd FROM usage_daily \
             WHERE project_id = ? ORDER BY day",
        )
        .bind(project.to_string())
        .fetch_all(meta.reader())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn judge_usage_is_summed_and_flushed() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add_judge(org, project, DAY_ONE, 100, 20, 0.25);
        usage.add_judge(org, project, DAY_ONE, 50, 5, 0.5);
        usage.flush(&meta).await.unwrap();
        let rows = stored_judge(&meta, project).await;
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].0, rows[0].1), (150, 25));
        assert!((rows[0].2 - 0.75).abs() < 1e-12, "{rows:?}");
    }

    #[tokio::test]
    async fn zero_judge_usage_adds_nothing() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add_judge(org, project, DAY_ONE, 0, 0, 0.0);
        usage.flush(&meta).await.unwrap();
        assert!(stored(&meta, project).await.is_empty());
    }

    #[tokio::test]
    async fn judge_and_span_counts_share_a_row() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add(org, project, DAY_ONE, 2, 0, 0);
        usage.add_judge(org, project, DAY_ONE, 7, 3, 0.1);
        usage.flush(&meta).await.unwrap();
        assert_eq!(
            stored(&meta, project).await,
            vec![("2026-01-01".to_owned(), 2, 0, 0)]
        );
        let judge = stored_judge(&meta, project).await;
        assert_eq!(judge.len(), 1);
        assert_eq!(judge[0].0, 7);
    }

    #[tokio::test]
    async fn a_failed_flush_keeps_the_counts_for_the_next_one() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add(org, project, DAY_ONE, 2, 10, 1);
        meta.write(|conn| {
            Box::pin(async move {
                sqlx::query("ALTER TABLE usage_daily RENAME TO usage_held")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        assert!(usage.flush(&meta).await.is_err());

        meta.write(|conn| {
            Box::pin(async move {
                sqlx::query("ALTER TABLE usage_held RENAME TO usage_daily")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        usage.flush(&meta).await.unwrap();
        assert_eq!(
            stored(&meta, project).await,
            vec![("2026-01-01".to_owned(), 2, 10, 1)]
        );
    }

    #[tokio::test]
    async fn a_non_finite_judge_cost_counts_as_zero() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add_judge(org, project, DAY_ONE, 10, 2, f64::NAN);
        usage.add_judge(org, project, DAY_ONE, 1, 1, f64::INFINITY);
        usage.add_judge(org, project, DAY_ONE, 1, 1, f64::NEG_INFINITY);
        usage.add_judge(org, project, DAY_ONE, 1, 1, -0.5);
        usage.flush(&meta).await.unwrap();
        let rows = stored_judge(&meta, project).await;
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].0, rows[0].1), (13, 5));
        assert_eq!(rows[0].2, 0.0, "{rows:?}");
    }

    #[tokio::test]
    async fn a_failed_flush_keeps_judge_usage_for_the_next_one() {
        let (_dir, meta) = test_db().await;
        let (org, project) = (OrgId::new_v7(), ProjectId::new_v7());
        let usage = Usage::default();
        usage.add(org, project, DAY_ONE, 2, 10, 1);
        usage.add_judge(org, project, DAY_ONE, 5, 7, 0.25);
        meta.write(|conn| {
            Box::pin(async move {
                sqlx::query("ALTER TABLE usage_daily RENAME TO usage_held")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        assert!(usage.flush(&meta).await.is_err());

        meta.write(|conn| {
            Box::pin(async move {
                sqlx::query("ALTER TABLE usage_held RENAME TO usage_daily")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        usage.flush(&meta).await.unwrap();
        assert_eq!(
            stored(&meta, project).await,
            vec![("2026-01-01".to_owned(), 2, 10, 1)]
        );
        let judge = stored_judge(&meta, project).await;
        assert_eq!(judge.len(), 1);
        assert_eq!((judge[0].0, judge[0].1), (5, 7));
        assert!((judge[0].2 - 0.25).abs() < 1e-12, "{judge:?}");
    }
}
