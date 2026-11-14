//! Usage counters (spec 04 § Metering, D-G-11, D-G-30).
//!
//! [`Usage::add`] sums accepted spans, request bytes and appended scores per org, project
//! and UTC day. [`Usage::flush`] upserts the sums into `usage_daily` and clears them.
//! `llmobs serve` flushes every 10 s and once at shutdown.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate};
use llmobs_core::ids::{OrgId, ProjectId};
use sqlx::SqliteConnection;

use crate::meta::MetaDb;
use crate::meta::MetaError;

/// Deletes every `usage_daily` row of the org (org drop, D-M-7).
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_org_usage(
    conn: &mut SqliteConnection,
    org: OrgId,
) -> Result<(), MetaError> {
    sqlx::query("DELETE FROM usage_daily WHERE org_id = ?")
        .bind(org.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// One `usage_daily` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UsageKey {
    org_id: OrgId,
    project_id: ProjectId,
    day: NaiveDate,
}

/// The counts of one `usage_daily` row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct UsageCounts {
    spans: u64,
    bytes: u64,
    scores: u64,
}

impl UsageCounts {
    fn merge(&mut self, other: Self) {
        self.spans = self.spans.saturating_add(other.spans);
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.scores = self.scores.saturating_add(other.scores);
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
        let key = UsageKey {
            org_id,
            project_id,
            day: DateTime::from_timestamp_nanos(now_ns).date_naive(),
        };
        self.lock().entry(key).or_default().merge(UsageCounts {
            spans,
            bytes,
            scores,
        });
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
                            "INSERT INTO usage_daily (org_id, project_id, day, spans, bytes, scores) \
                             VALUES (?, ?, ?, ?, ?, ?) \
                             ON CONFLICT (org_id, project_id, day) DO UPDATE SET \
                             spans = spans + excluded.spans, \
                             bytes = bytes + excluded.bytes, \
                             scores = scores + excluded.scores",
                        )
                        .bind(key.org_id.to_string())
                        .bind(key.project_id.to_string())
                        .bind(key.day.format("%Y-%m-%d").to_string())
                        .bind(i64::try_from(counts.spans).unwrap_or(i64::MAX))
                        .bind(i64::try_from(counts.bytes).unwrap_or(i64::MAX))
                        .bind(i64::try_from(counts.scores).unwrap_or(i64::MAX))
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
}
