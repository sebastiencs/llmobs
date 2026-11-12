//! Removes staged objects that were written and never committed.
//!
//! The task runs once at open, after replay, and then every [`INTERVAL`].
//! It reads `staged_objects` instead of listing the bucket. A row younger than
//! [`MAX_AGE`] stays, so a flush that has staged its outputs and not yet
//! committed them is left alone.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::catalog::{Committer, StagedRemoval, now_ns};
use crate::error::GcError;
use crate::hooks::Hooks;
use crate::objects::Objects;

/// How often the background task collects, after the pass at open.
pub(crate) const INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Staged rows older than this are eligible. Spec 03 §5.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// A running collection task. Drop cancels it.
#[derive(Debug)]
pub(crate) struct GcHandle {
    stop: watch::Sender<bool>,
    task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl GcHandle {
    /// Asks the task to stop and cancels it.
    pub(crate) fn abort(&self) {
        let _ = self.stop.send(true);
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

/// What the task needs. One value, so `spawn` stays under the argument limit.
pub(crate) struct GcStart {
    /// Catalog writer. Collection takes its connection for the row delete.
    pub committer: Arc<Committer>,
    /// Objects named by `staged_objects`.
    pub objects: Arc<Objects>,
    /// Test pause before a delete.
    pub hooks: Hooks,
}

/// Starts the periodic task. The caller already ran one pass at open.
pub(crate) fn spawn(start: GcStart) -> GcHandle {
    let (stop_tx, mut stop) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
        loop {
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    if let Err(error) = collect_once(
                        &start.committer,
                        &start.objects,
                        &start.hooks,
                        now_ns(),
                        MAX_AGE,
                    )
                    .await
                    {
                        // The error names the object key. Keys contain project ids.
                        tracing::warn!("staged object collection failed");
                        tracing::debug!(%error, "staged object collection error");
                    }
                }
            }
        }
    });
    GcHandle {
        stop: stop_tx,
        task: std::sync::Mutex::new(Some(task)),
    }
}

/// Deletes staged objects with `created_at` strictly before `now - max_age`.
pub(crate) async fn collect_once(
    committer: &Committer,
    objects: &Objects,
    hooks: &Hooks,
    now: i64,
    max_age: Duration,
) -> Result<(), GcError> {
    let cutoff = cutoff_ns(now, max_age);
    let paths = committer.staged_before(cutoff).await?;
    let mut deleted = 0_u64;
    let mut bytes = 0_u64;
    let mut conflicts = 0_u64;
    for path in &paths {
        match committer
            .collect_staged_path(path, hooks, || objects.delete_len(path))
            .await?
        {
            StagedRemoval::Deleted { bytes: size } => {
                deleted += 1;
                bytes = bytes.saturating_add(size);
                tracing::debug!(path, "deleted staged object");
            }
            StagedRemoval::Published => {
                conflicts += 1;
                tracing::error!("staged object is also a catalog file; left in place");
                tracing::debug!(path, "staged path is present in data_files");
            }
            StagedRemoval::Gone => {}
        }
    }
    if deleted > 0 || conflicts > 0 {
        tracing::info!(deleted, bytes, conflicts, "collected staged objects");
    }
    Ok(())
}

fn cutoff_ns(now: i64, max_age: Duration) -> i64 {
    let age = i64::try_from(max_age.as_nanos()).unwrap_or(i64::MAX);
    now.saturating_sub(age)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use llmobs_core::ids::ProjectId;

    use super::collect_once;
    use crate::catalog::now_ns;
    use crate::config::{
        FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };
    use crate::hooks::HookPoint;
    use crate::wal::{Table, spans_at};

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
        }
    }

    fn write_object(dir: &Path, path: &str, bytes: &[u8]) {
        let file = dir.join("objects").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }

    fn object_exists(dir: &Path, path: &str) -> bool {
        dir.join("objects").join(path).exists()
    }

    async fn collect(storage: &crate::Storage, now: i64, max_age: Duration) {
        collect_once(
            &storage.committer,
            &storage.objects,
            &storage.hooks,
            now,
            max_age,
        )
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_old_object_is_collected_and_a_young_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let now = now_ns();
        let old = "tables/spans/old.parquet";
        let young = "tables/spans/young.parquet";
        write_object(dir.path(), old, b"old-bytes");
        write_object(dir.path(), young, b"young-bytes");
        storage
            .committer
            .stage_at(&[old.to_owned()], now - 2_000_000_000)
            .await
            .unwrap();
        storage
            .committer
            .stage_at(&[young.to_owned()], now)
            .await
            .unwrap();

        collect(&storage, now, Duration::from_secs(1)).await;

        assert_eq!(
            storage.staged_paths().await.unwrap(),
            vec![young.to_owned()]
        );
        assert!(!object_exists(dir.path(), old));
        assert!(object_exists(dir.path(), young));
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_staged_row_without_an_object_is_collected() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let now = now_ns();
        let path = "tables/spans/missing.parquet";
        storage
            .committer
            .stage_at(&[path.to_owned()], now - 2_000_000_000)
            .await
            .unwrap();

        collect(&storage, now, Duration::from_secs(1)).await;

        assert!(storage.staged_paths().await.unwrap().is_empty());
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_catalog_file_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        let storage = crate::Storage::open(config).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        let started = Instant::now();
        let path = loop {
            let files = storage.catalog_files(Table::Spans);
            if files.len() == 1 && storage.published_rows(Table::Spans) == 0 {
                break files[0].path.clone();
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not publish the span"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let now = now_ns();
        storage
            .committer
            .stage_at(std::slice::from_ref(&path), now - 2_000_000_000)
            .await
            .unwrap();

        collect(&storage, now, Duration::from_secs(1)).await;

        assert!(object_exists(dir.path(), &path));
        assert_eq!(storage.staged_paths().await.unwrap(), vec![path]);
        assert_eq!(storage.catalog_rows(Table::Spans), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn collection_during_a_flush_keeps_the_fresh_staged_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        let storage = crate::Storage::open(config).await.unwrap();
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::FlushAfterStage);
        storage
            .append_spans(project(), spans_at(&[1, 2]))
            .await
            .unwrap();
        let started = Instant::now();
        while storage.hooks().waiting_at(HookPoint::FlushAfterStage) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not stage"
            );
            tokio::task::yield_now().await;
        }
        // One partition now stages a spans file and its derived traces file.
        let fresh = storage.staged_paths().await.unwrap();
        assert_eq!(fresh.len(), 2);

        let now = now_ns();
        let old = "tables/spans/leftover.parquet";
        write_object(dir.path(), old, b"leftover");
        storage
            .committer
            .stage_at(&[old.to_owned()], now - 2_000_000_000)
            .await
            .unwrap();
        collect(&storage, now, Duration::from_secs(1)).await;

        let mut staged = storage.staged_paths().await.unwrap();
        staged.retain(|path| path != old);
        assert_eq!(staged, fresh);
        assert!(!object_exists(dir.path(), old));
        hooks.release(HookPoint::FlushAfterStage);
        storage.shutdown().await.unwrap();

        let object = dir.path().join("objects").join(&fresh[0]);
        assert!(
            object.is_file(),
            "flush output was deleted during collection"
        );
    }
}
