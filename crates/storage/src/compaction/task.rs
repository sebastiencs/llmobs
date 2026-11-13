//! The background compaction task. It mirrors [`crate::gc`].

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::catalog::now_ns;
use crate::compaction::job::Compactor;

/// A running compaction task. Drop cancels it.
#[derive(Debug)]
pub(crate) struct CompactionHandle {
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl CompactionHandle {
    /// Asks the task to stop and cancels it.
    pub(crate) fn abort(&self) {
        let _ = self.stop.send(true);
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }

    /// Asks the task to stop and waits for the running pass to finish, for at
    /// most `limit`. On timeout the task is aborted.
    pub(crate) async fn stop_and_wait(&self, limit: Duration) {
        let _ = self.stop.send(true);
        let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let abort = task.abort_handle();
        if tokio::time::timeout(limit, task).await.is_err() {
            abort.abort();
        }
    }
}

/// Starts the periodic task. Each tick runs one pass at the wall clock.
pub(crate) fn spawn(compactor: std::sync::Arc<Compactor>, interval: Duration) -> CompactionHandle {
    let (stop_tx, mut stop) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    match compactor.pass(now_ns()).await {
                        Ok(report) if report.jobs + report.failed > 0 => {
                            tracing::debug!(
                                jobs = report.jobs,
                                failed = report.failed,
                                input_files = report.input_files,
                                output_files = report.output_files,
                                input_bytes = report.input_bytes,
                                output_bytes = report.output_bytes,
                                "compaction pass"
                            );
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(%error, "compaction pass failed");
                        }
                    }
                    match compactor.deletion_pass(now_ns()).await {
                        Ok(report)
                            if report.rewritten + report.raised + report.applied + report.failed
                                > 0 =>
                        {
                            tracing::debug!(
                                rewritten = report.rewritten,
                                raised = report.raised,
                                applied = report.applied,
                                failed = report.failed,
                                "deletion pass"
                            );
                        }
                        Ok(_) => {}
                        Err(error) => {
                            // The label only: a DataFusion error raised by a
                            // plan holding the covered-row predicate may quote
                            // its literals (spec 03 §7).
                            tracing::warn!(error = error.label(), "deletion pass failed");
                        }
                    }
                }
            }
        }
    });
    CompactionHandle {
        stop: stop_tx,
        task: Mutex::new(Some(task)),
    }
}
