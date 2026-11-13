//! Media blobs: the collection of unreferenced ones (spec 03 §9, D-A1-6).
//!
//! [`crate::Storage::put_media`] and [`crate::Storage::get_media`] live on
//! `Storage`; this module holds the blob types and [`MediaGc`], the collection
//! the background GC task, the compaction task and
//! [`crate::Storage::apply_deletions`] share.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::{Array, ListArray, RecordBatch, StringArray};
use bytes::Bytes;
use datafusion::error::DataFusionError;
use datafusion::execution::context::{SessionConfig, SessionContext};
use datafusion::execution::runtime_env::RuntimeEnv;
use llmobs_core::ids::ProjectId;

use crate::catalog::Committer;
use crate::error::{MediaError, QueryError};
use crate::hooks::Hooks;
use crate::objects::Objects;
use crate::read_state::Published;
use crate::{QueryOptions, project_query};

/// One extracted media blob (spec 02 § Media).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaBlob {
    /// SHA-256 of `bytes`. [`crate::Storage::put_media`] checks it.
    pub sha256: [u8; 32],
    /// MIME type, ASCII, 1 to 255 bytes.
    pub mime_type: String,
    /// The bytes. Never empty.
    pub bytes: Bytes,
}

/// [`crate::Storage::get_media`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRead {
    /// The stored MIME type.
    pub mime_type: String,
    /// Size of the whole blob, whatever the range.
    pub size: u64,
    /// The requested bytes.
    pub bytes: Bytes,
}

/// What one media collection did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaReport {
    /// Objects deleted, with their rows.
    pub deleted: u64,
    /// Bytes of the deleted objects.
    pub bytes: u64,
}

/// Blobs are kept this long after their last `put_media`, so a span appended
/// right after its blob is never raced (spec 03 §9).
pub(crate) const MEDIA_GRACE: Duration = Duration::from_secs(60 * 60);

/// Interval of the background media collection.
pub(crate) const MEDIA_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What a collection needs. Cloned into the GC and compaction tasks.
#[derive(Clone)]
pub(crate) struct MediaGc {
    pub(crate) committer: Arc<Committer>,
    pub(crate) objects: Arc<Objects>,
    pub(crate) published: Arc<Published>,
    pub(crate) runtime: Arc<RuntimeEnv>,
    pub(crate) target_partitions: NonZeroUsize,
    /// `query.trace_lookback` in ns, for [`project_query`].
    pub(crate) lookback: i64,
    pub(crate) hooks: Hooks,
}

impl std::fmt::Debug for MediaGc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MediaGc")
    }
}

impl MediaGc {
    /// One project's unreferenced blobs (spec 03 §9): every hash whose row is
    /// stale (`last_seen_at` more than [`MEDIA_GRACE`] before `now`) and that
    /// no `spans` row of the project references. Rows hidden by tombstones
    /// don't count: the live set goes through the same read path as queries.
    ///
    /// Errors are logged by the callers as `error = error.label()`: the error
    /// text may name an object path, which holds the blob's hash.
    pub(crate) async fn collect(
        &self,
        project: ProjectId,
        now: i64,
    ) -> Result<MediaReport, MediaError> {
        let grace = i64::try_from(MEDIA_GRACE.as_nanos()).unwrap_or(i64::MAX);
        let before = now.saturating_sub(grace);
        let live = self.live_set(project).await?;
        let mut report = MediaReport::default();
        for sha in self.committer.stale_media(project, before).await? {
            if live.contains(&sha) {
                continue;
            }
            let path = Objects::media_path(project, &sha);
            let Some(bytes) = self
                .committer
                .collect_media(project, &sha, before, &self.hooks, || {
                    self.objects.delete_len(&path)
                })
                .await?
            else {
                continue;
            };
            report.deleted += 1;
            report.bytes = report.bytes.saturating_add(bytes);
        }
        if report.deleted > 0 {
            tracing::info!(
                project = %project,
                deleted = report.deleted,
                bytes = report.bytes,
                "media collected"
            );
        }
        Ok(report)
    }

    /// Every project with media rows, summed. A project whose collection fails
    /// is logged (`error = error.label()`) and skipped, and retried on the next
    /// run; the others are still collected. Only reading the project list can
    /// fail the whole call.
    pub(crate) async fn collect_all(&self, now: i64) -> Result<MediaReport, MediaError> {
        let mut report = MediaReport::default();
        for project in self.committer.media_projects().await? {
            match self.collect(project, now).await {
                Ok(one) => {
                    report.deleted += one.deleted;
                    report.bytes = report.bytes.saturating_add(one.bytes);
                }
                Err(error) => {
                    // The label only: the error names an object path, which
                    // holds the blob's hash (spec 03 §9).
                    tracing::warn!(
                        project = %project,
                        error = error.label(),
                        "media collection failed"
                    );
                }
            }
        }
        Ok(report)
    }

    /// Every hash the project's `spans` rows reference, from one published
    /// snapshot through the query providers (spec 03 §9).
    async fn live_set(&self, project: ProjectId) -> Result<HashSet<String>, MediaError> {
        let state = self.published.load();
        let query = project_query(
            &state,
            project,
            QueryOptions::default(),
            &self.runtime,
            self.target_partitions,
            self.lookback,
        )
        .map_err(|error| match error {
            QueryError::DataFusion(error) => MediaError::Query(error),
        })?;
        let context = SessionContext::new_with_config_rt(
            SessionConfig::new().with_target_partitions(self.target_partitions.get()),
            Arc::clone(&self.runtime),
        );
        let batches = context
            .read_table(Arc::clone(&query.spans))
            .map_err(MediaError::from)?
            .select_columns(&["media_refs"])
            .map_err(MediaError::from)?
            .collect()
            .await
            .map_err(MediaError::from)?;
        let mut live = HashSet::new();
        for batch in &batches {
            insert_media_refs(batch, &mut live)?;
        }
        Ok(live)
    }
}

/// Every string of `batch`'s single `media_refs` list column into `live`.
fn insert_media_refs(batch: &RecordBatch, live: &mut HashSet<String>) -> Result<(), MediaError> {
    let not_a_list = || {
        MediaError::Query(DataFusionError::Internal(
            "the media_refs column is not a string list".to_owned(),
        ))
    };
    let column = batch
        .column(0)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(not_a_list)?;
    for row in 0..batch.num_rows() {
        if column.is_null(row) {
            continue;
        }
        let values = column.value(row);
        let strings = values
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(not_a_list)?;
        for index in 0..strings.len() {
            if !strings.is_null(index) {
                live.insert(strings.value(index).to_owned());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use llmobs_core::ids::ProjectId;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::{MediaBlob, MediaError, MediaReport};
    use crate::Storage;
    use crate::catalog::now_ns;
    use crate::compaction::job::tests::{at, project, quiet_compaction};
    use crate::deletion::{Deletion, DeletionScope};
    use crate::hooks::HookPoint;
    use crate::objects::Objects;
    use crate::test_support::{span_row, span_rows};

    /// One hour, the media grace, in ns.
    const HOUR_NS: i64 = 3_600_000_000_000;

    /// The second fixed test project, after `project()` in id order.
    fn other_project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
    }

    fn sha256_of(bytes: &Bytes) -> [u8; 32] {
        Sha256::digest(bytes.as_ref()).into()
    }

    fn blob_of(bytes: &[u8], mime_type: &str) -> MediaBlob {
        let bytes = Bytes::copy_from_slice(bytes);
        MediaBlob {
            sha256: sha256_of(&bytes),
            mime_type: mime_type.to_owned(),
            bytes,
        }
    }

    /// The blob's object path under the local object root.
    fn object_file(dir: &Path, project: ProjectId, sha: &[u8; 32]) -> PathBuf {
        dir.join("objects")
            .join(Objects::media_path(project, &hex::encode(sha)))
    }

    /// Files under `objects/media/`, all projects: two directory levels.
    fn media_objects(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(projects) = std::fs::read_dir(dir.join("objects").join("media")) else {
            return found;
        };
        for project in projects.flatten() {
            if let Ok(files) = std::fs::read_dir(project.path()) {
                found.extend(files.flatten().map(|file| file.path()));
            }
        }
        found
    }

    /// `(first_seen_at, last_seen_at)` of the blob's `media` row.
    async fn seen_at(dir: &Path, project: ProjectId, sha: &[u8; 32]) -> (i64, i64) {
        use sqlx::ConnectOptions;
        let mut connection = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.join("catalog.db"))
            .read_only(true)
            .connect()
            .await
            .unwrap();
        sqlx::query_as(
            "SELECT first_seen_at, last_seen_at FROM media WHERE project_id = ? AND sha256 = ?",
        )
        .bind(project.to_string())
        .bind(hex::encode(sha))
        .fetch_one(&mut connection)
        .await
        .unwrap()
    }

    async fn media_rows(dir: &Path) -> i64 {
        use sqlx::ConnectOptions;
        let mut connection = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.join("catalog.db"))
            .read_only(true)
            .connect()
            .await
            .unwrap();
        sqlx::query_scalar("SELECT COUNT(*) FROM media")
            .fetch_one(&mut connection)
            .await
            .unwrap()
    }

    fn config_in(dir: &Path) -> crate::config::StorageConfig {
        crate::compaction::job::tests::config_in(
            dir,
            quiet_compaction(8),
            Duration::from_secs(3_600),
        )
    }

    // 1. A put followed by a get returns the bytes and the MIME type, whole or
    //    as a range.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn put_then_get_round_trips_whole_and_ranged() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let blob = blob_of(b"media-bytes-0123456789", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();

        let read = storage.get_media(project(), &sha, None).await.unwrap();
        assert_eq!(read.mime_type, "image/png");
        assert_eq!(read.bytes, Bytes::from_static(b"media-bytes-0123456789"));
        assert_eq!(read.size, 22);
        let object = object_file(dir.path(), project(), &sha);
        assert_eq!(
            std::fs::read(&object).unwrap(),
            b"media-bytes-0123456789".to_vec(),
            "the object is not at {object:?}"
        );

        let ranged = storage
            .get_media(project(), &sha, Some(2..5))
            .await
            .unwrap();
        assert_eq!(ranged.bytes, Bytes::from_static(b"dia"));
        assert_eq!(ranged.size, 22, "the range keeps the whole blob's size");
        storage.shutdown().await.unwrap();
    }

    // 2. A wrong hash, a bad MIME type, an empty blob, an unknown hash and a
    //    range outside the blob are refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn invalid_blobs_unknown_hashes_and_bad_ranges_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();

        let mut wrong_hash = blob_of(b"some media", "image/png");
        wrong_hash.sha256 = [1; 32];
        assert!(
            matches!(
                storage.put_media(project(), wrong_hash).await,
                Err(MediaError::HashMismatch)
            ),
            "a wrong hash passed"
        );
        assert!(
            media_objects(dir.path()).is_empty(),
            "an object was written"
        );
        assert_eq!(media_rows(dir.path()).await, 0, "a row was written");

        for mime_type in ["", &"x".repeat(256), "imäge/png"] {
            let blob = blob_of(b"some media", mime_type);
            assert!(
                matches!(
                    storage.put_media(project(), blob).await,
                    Err(MediaError::Invalid {
                        reason: "mime type"
                    })
                ),
                "{mime_type:?} passed"
            );
        }
        assert!(
            matches!(
                storage
                    .put_media(project(), blob_of(b"", "image/png"))
                    .await,
                Err(MediaError::Invalid {
                    reason: "empty blob"
                })
            ),
            "an empty blob passed"
        );
        assert_eq!(media_rows(dir.path()).await, 0);
        assert!(media_objects(dir.path()).is_empty());

        let unknown = [7; 32];
        assert!(
            matches!(
                storage.get_media(project(), &unknown, None).await,
                Err(MediaError::NotFound)
            ),
            "an unknown hash was found"
        );

        let blob = blob_of(b"0123456789", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();
        for range in [Some(5..5), Some(0..11), Some(11..12)] {
            assert!(
                matches!(
                    storage.get_media(project(), &sha, range.clone()).await,
                    Err(MediaError::InvalidRange)
                ),
                "{range:?} passed"
            );
        }
        storage.shutdown().await.unwrap();
    }

    // 3. A second put of a stored hash writes nothing new and raises
    //    `last_seen_at`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_put_touches_only_last_seen_at() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let blob = blob_of(b"idempotent-media", "image/png");
        let sha = blob.sha256;
        let first_at = now_ns();
        storage.put_media(project(), blob.clone()).await.unwrap();
        let (first, last) = seen_at(dir.path(), project(), &sha).await;
        assert_eq!(first, last);
        assert!(first >= first_at);

        tokio::time::sleep(Duration::from_millis(5)).await;
        storage.put_media(project(), blob).await.unwrap();
        let (first_again, last_again) = seen_at(dir.path(), project(), &sha).await;
        assert_eq!(first_again, first, "first_seen_at moved");
        assert!(last_again > last, "last_seen_at did not rise");
        assert_eq!(media_rows(dir.path()).await, 1);
        assert_eq!(media_objects(dir.path()).len(), 1);
        assert!(storage.staged_paths().await.unwrap().is_empty());
        storage.shutdown().await.unwrap();
    }

    // 4. Eight concurrent puts of one blob leave one row, one object and no
    //    staged row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_puts_leave_one_row_and_one_object() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(config_in(dir.path())).await.unwrap());
        let blob = blob_of(b"concurrent-media", "image/png");
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let storage = Arc::clone(&storage);
            let blob = blob.clone();
            tasks.push(tokio::spawn(async move {
                storage.put_media(project(), blob).await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(media_rows(dir.path()).await, 1);
        assert_eq!(media_objects(dir.path()).len(), 1);
        assert!(storage.staged_paths().await.unwrap().is_empty());
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    // 5. A crash between the put and the insert leaves the object staged;
    //    staged-object GC deletes it and its row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_fault_after_the_put_leaves_the_object_for_staged_gc() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let blob = blob_of(b"crashed-media", "image/png");
        let sha = blob.sha256;
        storage.hooks().arm_fail(HookPoint::MediaAfterPut);
        assert!(
            matches!(
                storage.put_media(project(), blob).await,
                Err(MediaError::Fault)
            ),
            "the fault did not fail the put"
        );
        let path = Objects::media_path(project(), &hex::encode(sha));
        assert!(dir.path().join("objects").join(&path).is_file());
        assert_eq!(storage.staged_paths().await.unwrap(), vec![path.clone()]);
        assert_eq!(media_rows(dir.path()).await, 0);

        storage.collect_staged(Duration::ZERO).await.unwrap();
        assert!(!dir.path().join("objects").join(&path).exists());
        assert!(storage.staged_paths().await.unwrap().is_empty());
        storage.shutdown().await.unwrap();
    }

    // 6. A stale staged row of a stored blob: collection removes only the
    //    staged row; the object, the `media` row and `get_media` stay.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stale_staged_row_of_a_stored_blob_is_removed_without_the_object() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let blob = blob_of(b"stored-media", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();
        let path = Objects::media_path(project(), &hex::encode(sha));
        // What a crashed second put would have left behind.
        storage.committer.stage_media(&path, 1).await.unwrap();

        storage.collect_staged(Duration::ZERO).await.unwrap();
        assert!(
            storage.staged_paths().await.unwrap().is_empty(),
            "the staged row stayed"
        );
        assert!(
            dir.path().join("objects").join(&path).is_file(),
            "the object was deleted"
        );
        assert_eq!(media_rows(dir.path()).await, 1);
        let read = storage.get_media(project(), &sha, None).await.unwrap();
        assert_eq!(read.bytes, Bytes::from_static(b"stored-media"));
        storage.shutdown().await.unwrap();
    }

    // 9a. A blob referenced by a span in a file is kept however stale.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_blob_referenced_by_a_span_in_a_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let blob = blob_of(b"file-referenced-media", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();
        let time = at(0, 0, 0);
        let mut span = span_row(1, 0, time, time);
        span.media_refs = vec![hex::encode(sha)];
        storage
            .append_spans(project(), span_rows(&[span]))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.catalog_rows(crate::wal::Table::Spans), 1);
        assert_eq!(storage.published_rows(crate::wal::Table::Spans), 0);
        let report = storage.collect_media(now_ns() + 2 * HOUR_NS).await.unwrap();
        assert_eq!(report, MediaReport::default());
        let read = storage.get_media(project(), &sha, None).await.unwrap();
        assert_eq!(read.bytes, Bytes::from_static(b"file-referenced-media"));
        storage.shutdown().await.unwrap();
    }

    // 9b. A blob referenced only by a memtable span is kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_blob_referenced_only_by_a_memtable_span_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let blob = blob_of(b"memtable-media", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();
        let time = at(0, 0, 0);
        let mut span = span_row(1, 0, time, time);
        span.media_refs = vec![hex::encode(sha)];
        storage
            .append_spans(project(), span_rows(&[span]))
            .await
            .unwrap();
        assert_eq!(storage.published_rows(crate::wal::Table::Spans), 1);

        let report = storage.collect_media(now_ns() + 2 * HOUR_NS).await.unwrap();
        assert_eq!(report, MediaReport::default());
        storage.get_media(project(), &sha, None).await.unwrap();
        storage.shutdown().await.unwrap();
    }

    // 9c. An unreferenced blob older than the grace is deleted and counted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stale_unreferenced_blob_is_deleted_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let bytes = b"orphan-media";
        let blob = blob_of(bytes, "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();

        let report = storage.collect_media(now_ns() + 2 * HOUR_NS).await.unwrap();
        assert_eq!(
            report,
            MediaReport {
                deleted: 1,
                bytes: u64::try_from(bytes.len()).unwrap(),
            }
        );
        assert!(!object_file(dir.path(), project(), &sha).exists());
        assert_eq!(media_rows(dir.path()).await, 0);
        assert!(
            matches!(
                storage.get_media(project(), &sha, None).await,
                Err(MediaError::NotFound)
            ),
            "the row survived its collection"
        );
        storage.shutdown().await.unwrap();
    }

    // 9d. An unreferenced blob seen within the grace is kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_unreferenced_blob_seen_within_the_grace_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let blob = blob_of(b"fresh-media", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();

        let report = storage.collect_media(now_ns()).await.unwrap();
        assert_eq!(report, MediaReport::default());
        assert!(object_file(dir.path(), project(), &sha).is_file());
        assert_eq!(media_rows(dir.path()).await, 1);
        storage.shutdown().await.unwrap();
    }

    // 9e. A project whose collection fails does not block the others: the
    //     error is logged and its blobs retried on the next run. The one-shot
    //     `MediaBeforeDelete` fault hits the first project in id order; it
    //     fires after the row delete committed, so the leftover is a staged
    //     row with the object, which staged-object GC removes (spec 03 §9).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failing_project_does_not_block_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let other = other_project();
        let mine = blob_of(b"failed-project-media", "image/png");
        let theirs = blob_of(b"kept-project-media", "image/png");
        let mine_sha = mine.sha256;
        let theirs_sha = theirs.sha256;
        storage.put_media(project(), mine).await.unwrap();
        storage.put_media(other, theirs).await.unwrap();
        storage.hooks().arm_fail(HookPoint::MediaBeforeDelete);

        let now = now_ns() + 2 * HOUR_NS;
        let report = storage.collect_media(now).await.unwrap();
        assert_eq!(report.deleted, 1, "the other project was not collected");
        assert_eq!(
            report.bytes,
            u64::try_from(b"kept-project-media".len()).unwrap()
        );
        assert!(!object_file(dir.path(), other, &theirs_sha).exists());
        assert!(object_file(dir.path(), project(), &mine_sha).is_file());
        // The fault fired after the failing project's row delete: only a
        // staged row remains of it.
        assert_eq!(media_rows(dir.path()).await, 0);
        let staged = storage.staged_paths().await.unwrap();
        assert_eq!(staged.len(), 1, "{staged:?}");

        // The leftover is collected by staged-object GC, and a later media
        // collection has nothing left to do.
        storage.collect_staged(Duration::ZERO).await.unwrap();
        assert!(!object_file(dir.path(), project(), &mine_sha).exists());
        assert!(storage.staged_paths().await.unwrap().is_empty());
        assert_eq!(
            storage.collect_media(now).await.unwrap(),
            MediaReport::default()
        );
        storage.shutdown().await.unwrap();
    }

    // 10. Applying a deletion collects the media its rows referenced: the pass
    //     runs one collection per applied project at its own `now`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_deletion_collects_the_media_of_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let blob = blob_of(b"deleted-media", "image/png");
        let sha = blob.sha256;
        storage.put_media(project(), blob).await.unwrap();
        let time = at(0, 0, 0);
        let mut span = span_row(1, 0, time, time);
        span.media_refs = vec![hex::encode(sha)];
        let trace_id = span.trace_id;
        storage
            .append_spans(project(), span_rows(&[span]))
            .await
            .unwrap();
        let deletion = Deletion {
            id: Uuid::now_v7(),
            project: project(),
            cutoff: time + 1,
            scope: DeletionScope::Traces {
                trace_ids: vec![trace_id],
            },
        };
        storage.register_deletion(deletion.clone()).await.unwrap();
        // The shutdown flush drops the covered rows; no file ever holds them.
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        let now = now_ns() + 2 * HOUR_NS;
        let report = storage.apply_deletions(now).await.unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(
            storage.deletion_status(deletion.id).await.unwrap(),
            crate::deletion::DeletionStatus::Applied { applied_at: now }
        );
        assert!(
            !object_file(dir.path(), project(), &sha).exists(),
            "the blob survived its deletion"
        );
        assert_eq!(media_rows(dir.path()).await, 0);
        assert!(
            matches!(
                storage.get_media(project(), &sha, None).await,
                Err(MediaError::NotFound)
            ),
            "the row survived its deletion"
        );
        storage.shutdown().await.unwrap();
    }
}
