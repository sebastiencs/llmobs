//! Where flushed Parquet files are stored, and how a local write becomes durable.
//!
//! `LocalFileSystem` in object_store 0.13 does not fsync. It is registered for
//! reads only. Local `put_durable` writes a temp file, `sync_all`s it, renames
//! it into place, and fsyncs the directory. Remote writes go through the store.
//!
//! `Storage::open` builds one [`Objects`], removes leftover local temp files,
//! and registers the store on the shared query runtime.

#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use chrono::NaiveDate;
use datafusion::execution::object_store::ObjectStoreUrl as RegisteredUrl;
use datafusion::execution::runtime_env::RuntimeEnv;
use llmobs_core::ids::ProjectId;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjectPath;
use object_store::prefix::PrefixStore;
use object_store::{MultipartUpload, ObjectStore, ObjectStoreExt, PutPayload};
use uuid::Uuid;

use crate::catalog::{FileId, FileTable};
use crate::config::{ObjectStoreUrl, S3Config, StorageConfig};
use crate::error::ObjectError;
use crate::hooks::{HookPoint, Hooks};
use crate::wal;

/// DataFusion keys a registered store by scheme, host, and port. The path is
/// not part of that key, and [`RegisteredUrl::parse`] rejects any path other
/// than empty or `/`.
///
/// File URLs used by the providers are `llmobs://objects/tables/...`.
pub(crate) const OBJECT_STORE_URL: &str = "llmobs://objects";

/// `put` is used up to this size, inclusive. Larger objects use multipart.
const SINGLE_PUT_MAX: usize = 64 * 1024 * 1024;

/// Size of every multipart part except the last.
const MULTIPART_PART: usize = 16 * 1024 * 1024;

/// Object storage for one opened data directory.
#[derive(Debug)]
pub(crate) struct Objects {
    kind: Kind,
    store: Arc<dyn ObjectStore>,
    url: RegisteredUrl,
    hooks: Hooks,
}

#[derive(Debug)]
enum Kind {
    /// `root` is the canonical directory. Writes do not go through `store`.
    Local { root: PathBuf },
    /// S3, or a test store standing in for it.
    Remote,
}

impl Objects {
    /// Opens the store from `config`.
    ///
    /// Local roots are created, including missing parents, and each new
    /// directory's parent is fsynced. Blocking work runs on `spawn_blocking`.
    pub(crate) async fn new(config: &StorageConfig, hooks: Hooks) -> Result<Self, ObjectError> {
        let url = registered_url()?;
        match config.object_store_url() {
            ObjectStoreUrl::Local(root) => {
                let error_path = root.clone();
                let prepared = tokio::task::spawn_blocking(move || prepare_local_root(&root)).await;
                let (root, store) = prepared.map_err(|source| {
                    ObjectError::io(
                        "opening object store",
                        &error_path,
                        io::Error::other(source),
                    )
                })??;
                Ok(Self {
                    kind: Kind::Local { root },
                    store: Arc::new(store),
                    url,
                    hooks,
                })
            }
            ObjectStoreUrl::S3 { bucket, prefix } => Ok(Self {
                kind: Kind::Remote,
                store: build_s3(&bucket, &prefix, &config.s3)?,
                url,
                hooks,
            }),
        }
    }

    /// Remote store for tests. `store` is usually [`crate::test_support::FaultStore`].
    #[cfg(test)]
    pub(crate) fn remote_for_test(
        store: Arc<dyn ObjectStore>,
        hooks: Hooks,
    ) -> Result<Self, ObjectError> {
        Ok(Self {
            kind: Kind::Remote,
            store,
            url: registered_url()?,
            hooks,
        })
    }

    /// Registers `store` under [`OBJECT_STORE_URL`].
    pub(crate) fn register(&self, runtime: &RuntimeEnv) {
        runtime.register_object_store(self.url.as_ref(), Arc::clone(&self.store));
    }

    /// The URL passed to [`Self::register`].
    pub(crate) fn url(&self) -> &RegisteredUrl {
        &self.url
    }

    /// The object store, for async Parquet reads at open.
    pub(crate) fn store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.store)
    }

    /// `tables/<table>/project=<project_id>/day=<YYYY-MM-DD>/<level>-<uuidv7>.parquet`.
    ///
    /// This is the only path formatter. `level` is `0` or `1`.
    pub(crate) fn table_file_path(
        table: FileTable,
        project: ProjectId,
        day: NaiveDate,
        level: u8,
    ) -> (String, FileId) {
        let id = FileId::new_v7();
        let path = format!(
            "tables/{}/project={project}/day={}/{level}-{id}.parquet",
            table.as_str(),
            day.format("%Y-%m-%d"),
        );
        (path, id)
    }

    /// Writes `bytes` so a later reader sees all of them or nothing.
    ///
    /// Local writes run on `spawn_blocking`. An existing final file is
    /// [`ObjectError::AlreadyExists`]. A remote error is [`ObjectError::Upload`]
    /// and is not retried here.
    pub(crate) async fn put_durable(&self, path: &str, bytes: Bytes) -> Result<(), ObjectError> {
        validate_key(path)?;
        match &self.kind {
            Kind::Local { root } => {
                let root = root.clone();
                let key = path.to_owned();
                let hooks = clone_hooks(&self.hooks);
                tokio::task::spawn_blocking(move || put_local(&root, &key, &bytes, &hooks))
                    .await
                    .map_err(|source| {
                        ObjectError::io("writing object", Path::new(path), io::Error::other(source))
                    })?
            }
            Kind::Remote => put_remote(&self.store, path, bytes).await,
        }
    }

    /// Deletes one object. A missing object is success. Used by GC.
    pub(crate) async fn delete(&self, path: &str) -> Result<(), ObjectError> {
        validate_key(path)?;
        let location = object_key(path)?;
        match self.store.delete(&location).await {
            Ok(()) => Ok(()),
            Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(source) => Err(ObjectError::store("deleting object", path, source)),
        }
    }

    /// Size of `path`, then [`Self::delete`]. A missing object is `0`.
    pub(crate) async fn delete_len(&self, path: &str) -> Result<u64, ObjectError> {
        validate_key(path)?;
        let location = object_key(path)?;
        let bytes = match self.store.head(&location).await {
            Ok(meta) => meta.size,
            Err(object_store::Error::NotFound { .. }) => 0,
            Err(source) => return Err(ObjectError::store("reading object metadata", path, source)),
        };
        self.delete(path).await?;
        Ok(bytes)
    }

    /// Deletes leftover local temp files under `tables/`.
    ///
    /// Remote stores return `0`. This walks the filesystem on the calling
    /// thread; M2.6 calls it from `spawn_blocking` at open.
    pub(crate) fn remove_local_temp_files(&self) -> Result<u64, ObjectError> {
        let Kind::Local { root } = &self.kind else {
            return Ok(0);
        };
        let mut removed = 0;
        remove_temps(&root.join("tables"), &mut removed)?;
        Ok(removed)
    }
}

#[allow(clippy::clone_on_copy)]
fn clone_hooks(hooks: &Hooks) -> Hooks {
    hooks.clone()
}

fn registered_url() -> Result<RegisteredUrl, ObjectError> {
    RegisteredUrl::parse(OBJECT_STORE_URL).map_err(|_| ObjectError::InvalidPath)
}

fn prepare_local_root(root: &Path) -> Result<(PathBuf, LocalFileSystem), ObjectError> {
    create_dir_chain(root)?;
    let root = root
        .canonicalize()
        .map_err(|source| ObjectError::io("opening object store", root, source))?;
    let store = LocalFileSystem::new_with_prefix(&root).map_err(ObjectError::client)?;
    Ok((root, store))
}

fn build_s3(
    bucket: &str,
    prefix: &str,
    s3: &S3Config,
) -> Result<Arc<dyn ObjectStore>, ObjectError> {
    let mut builder = AmazonS3Builder::from_env().with_bucket_name(bucket);
    if let Some(region) = &s3.region {
        builder = builder.with_region(region);
    }
    if let Some(endpoint) = &s3.endpoint {
        builder = builder.with_endpoint(endpoint);
    }
    // `force_path_style` is the config flag. The builder's flag is the opposite.
    builder = builder
        .with_virtual_hosted_style_request(!s3.force_path_style)
        .with_allow_http(s3.allow_http);
    let store = builder.build().map_err(ObjectError::client)?;
    if prefix.is_empty() {
        Ok(Arc::new(store))
    } else {
        Ok(Arc::new(PrefixStore::new(store, prefix)))
    }
}

fn put_local(root: &Path, key: &str, bytes: &Bytes, hooks: &Hooks) -> Result<(), ObjectError> {
    let final_path = root.join(key);
    if try_exists(&final_path)? {
        return Err(ObjectError::AlreadyExists {
            path: key.to_owned(),
        });
    }
    let Some(directory) = final_path.parent() else {
        return Err(ObjectError::InvalidPath);
    };
    let Ok(relative_dir) = directory.strip_prefix(root) else {
        return Err(ObjectError::InvalidPath);
    };
    create_dir_chain_from(root, relative_dir)?;

    let Some(file_name) = final_path.file_name() else {
        return Err(ObjectError::InvalidPath);
    };
    let temp_path = directory.join(temp_name(file_name));

    hooks.pause(HookPoint::ObjectWrite);
    if let Some(source) = hooks.fail(HookPoint::ObjectWrite) {
        return Err(ObjectError::io("writing object", &temp_path, source));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|source| ObjectError::io("writing object", &temp_path, source))?;
    file.write_all(bytes)
        .map_err(|source| ObjectError::io("writing object", &temp_path, source))?;

    hooks.pause(HookPoint::ObjectSync);
    if let Some(source) = hooks.fail(HookPoint::ObjectSync) {
        return Err(ObjectError::io("syncing object", &temp_path, source));
    }
    file.sync_all()
        .map_err(|source| ObjectError::io("syncing object", &temp_path, source))?;
    drop(file);

    hooks.pause(HookPoint::ObjectRename);
    if let Some(source) = hooks.fail(HookPoint::ObjectRename) {
        return Err(ObjectError::io("renaming object", &final_path, source));
    }
    if try_exists(&final_path)? {
        return Err(ObjectError::AlreadyExists {
            path: key.to_owned(),
        });
    }
    fs::rename(&temp_path, &final_path)
        .map_err(|source| ObjectError::io("renaming object", &final_path, source))?;

    hooks.pause(HookPoint::ObjectDirSync);
    if let Some(source) = hooks.fail(HookPoint::ObjectDirSync) {
        // The rename already published the final name. Move it back so a failed
        // `put_durable` does not leave that name, and startup cleanup can drop
        // the temp file. A real `sync_dir` error below is not undone: the bytes
        // are already under the final name.
        fs::rename(&final_path, &temp_path)
            .map_err(|undo| ObjectError::io("restoring object temp file", &temp_path, undo))?;
        return Err(ObjectError::io(
            "syncing object directory",
            directory,
            source,
        ));
    }
    wal::sync_dir(directory)
        .map_err(|source| ObjectError::io("syncing object directory", directory, source))?;
    Ok(())
}

async fn put_remote(
    store: &Arc<dyn ObjectStore>,
    path: &str,
    mut bytes: Bytes,
) -> Result<(), ObjectError> {
    let location = object_key(path)?;
    if bytes.len() <= SINGLE_PUT_MAX {
        return store
            .put(&location, PutPayload::from(bytes))
            .await
            .map_or_else(|source| Err(ObjectError::upload(path, source)), |_| Ok(()));
    }
    let mut upload = store
        .put_multipart(&location)
        .await
        .map_err(|source| ObjectError::upload(path, source))?;
    while !bytes.is_empty() {
        let part = bytes.split_to(MULTIPART_PART.min(bytes.len()));
        if let Err(source) = upload.put_part(PutPayload::from(part)).await {
            let _ = upload.abort().await;
            return Err(ObjectError::upload(path, source));
        }
    }
    // A failed complete is ambiguous: the object may be visible. Do not abort.
    upload
        .complete()
        .await
        .map_err(|source| ObjectError::upload(path, source))?;
    Ok(())
}

fn temp_name(file_name: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(".tmp-{}", Uuid::now_v7()));
    name
}

fn is_temp_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with('.') && name.contains(".tmp-")
}

fn remove_temps(dir: &Path, removed: &mut u64) -> Result<(), ObjectError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(ObjectError::io("reading object directory", dir, source)),
    };
    for entry in entries {
        let entry =
            entry.map_err(|source| ObjectError::io("reading object directory", dir, source))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|source| ObjectError::io("reading object directory", &path, source))?;
        if kind.is_dir() {
            remove_temps(&path, removed)?;
        } else if kind.is_file() && is_temp_name(&entry.file_name()) {
            fs::remove_file(&path)
                .map_err(|source| ObjectError::io("removing object temp file", &path, source))?;
            *removed += 1;
        }
    }
    Ok(())
}

/// Creates `path` and any missing parent. Each new directory's parent is fsynced.
fn create_dir_chain(path: &Path) -> Result<(), ObjectError> {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        if try_exists(cursor)? {
            if !cursor.is_dir() {
                return Err(ObjectError::io(
                    "creating object directory",
                    cursor,
                    io::Error::other("not a directory"),
                ));
            }
            break;
        }
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| {
            ObjectError::io(
                "creating object directory",
                path,
                io::Error::other("not a directory"),
            )
        })?;
    }
    for dir in missing.iter().rev() {
        match fs::create_dir(dir) {
            Ok(()) => {
                if let Some(parent) = dir.parent() {
                    wal::sync_dir(parent).map_err(|source| {
                        ObjectError::io("syncing object directory", parent, source)
                    })?;
                }
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
            Err(source) => {
                return Err(ObjectError::io("creating object directory", dir, source));
            }
        }
    }
    Ok(())
}

fn create_dir_chain_from(root: &Path, relative: &Path) -> Result<(), ObjectError> {
    let mut current = root.to_path_buf();
    for segment in relative.components() {
        current.push(segment);
        if try_exists(&current)? {
            if current.is_dir() {
                continue;
            }
            return Err(ObjectError::io(
                "creating object directory",
                &current,
                io::Error::other("not a directory"),
            ));
        }
        match fs::create_dir(&current) {
            Ok(()) => {
                if let Some(parent) = current.parent() {
                    wal::sync_dir(parent).map_err(|source| {
                        ObjectError::io("syncing object directory", parent, source)
                    })?;
                }
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists && current.is_dir() => {}
            Err(source) => {
                return Err(ObjectError::io(
                    "creating object directory",
                    &current,
                    source,
                ));
            }
        }
    }
    Ok(())
}

fn try_exists(path: &Path) -> Result<bool, ObjectError> {
    path.try_exists()
        .map_err(|source| ObjectError::io("checking object", path, source))
}

fn validate_key(path: &str) -> Result<(), ObjectError> {
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', '\0']) {
        return Err(ObjectError::InvalidPath);
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(ObjectError::InvalidPath);
    }
    object_key(path).map(|_| ())
}

fn object_key(path: &str) -> Result<ObjectPath, ObjectError> {
    ObjectPath::parse(path).map_err(|_| ObjectError::InvalidPath)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use chrono::NaiveDate;
    use datafusion::execution::runtime_env::RuntimeEnv;
    use llmobs_core::ids::ProjectId;
    use object_store::ObjectStoreExt;
    use object_store::memory::InMemory;

    use super::{MULTIPART_PART, OBJECT_STORE_URL, SINGLE_PUT_MAX, is_temp_name};
    use crate::catalog::FileTable;
    use crate::config::{
        FlushConfig, MemtableConfig, ObjectStoreUrl, QueryConfig, S3Config, StorageConfig,
        WalConfig,
    };
    use crate::error::ObjectError;
    use crate::hooks::{HookPoint, Hooks};
    use crate::objects::Objects;
    use crate::test_support::{FaultKind, FaultStore};

    fn local_config(dir: &std::path::Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            object_store: Some(ObjectStoreUrl::Local(dir.join("objects"))),
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
            gc: Default::default(),
        }
    }

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
    }

    async fn read_registered(objects: &Objects, key: &str) -> Bytes {
        let runtime = RuntimeEnv::default();
        objects.register(&runtime);
        let store = runtime.object_store(objects.url()).unwrap();
        let location = object_store::path::Path::parse(key).unwrap();
        store.get(&location).await.unwrap().bytes().await.unwrap()
    }

    fn temps(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    walk(&path, found);
                } else if kind.is_file() && is_temp_name(&entry.file_name()) {
                    found.push(path);
                }
            }
        }
        walk(&root.join("tables"), &mut found);
        found
    }

    #[tokio::test]
    async fn local_put_is_durable_and_readable() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::default();
        let objects = Objects::new(&local_config(dir.path()), hooks.clone())
            .await
            .unwrap();
        let (key, id) = Objects::table_file_path(FileTable::Spans, project(), day(), 0);
        let bytes = Bytes::from_static(b"parquet-bytes");
        objects.put_durable(&key, bytes.clone()).await.unwrap();

        let root = dir.path().join("objects");
        let final_path = root.join(&key);
        assert_eq!(std::fs::read(&final_path).unwrap(), bytes.as_ref());
        assert!(temps(&root).is_empty());
        assert_eq!(
            hooks.points(),
            vec![
                HookPoint::ObjectWrite,
                HookPoint::ObjectSync,
                HookPoint::ObjectRename,
                HookPoint::ObjectDirSync,
            ]
        );
        assert_eq!(read_registered(&objects, &key).await, bytes);
        assert!(key.contains(&id.to_string()));

        let again = objects.put_durable(&key, bytes).await.unwrap_err();
        assert!(matches!(again, ObjectError::AlreadyExists { .. }));

        objects.delete(&key).await.unwrap();
        assert!(!final_path.exists());
        objects.delete(&key).await.unwrap();
    }

    #[tokio::test]
    async fn local_faults_leave_no_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("objects");
        for point in [
            HookPoint::ObjectWrite,
            HookPoint::ObjectSync,
            HookPoint::ObjectRename,
            HookPoint::ObjectDirSync,
        ] {
            let hooks = Hooks::default();
            let objects = Objects::new(&local_config(dir.path()), hooks.clone())
                .await
                .unwrap();
            hooks.arm_fail(point);
            let (key, _) = Objects::table_file_path(FileTable::Scores, project(), day(), 1);
            let error = objects
                .put_durable(&key, Bytes::from_static(b"nope"))
                .await
                .unwrap_err();
            assert!(
                matches!(error, ObjectError::Io { .. }),
                "{point:?} returned {error}"
            );
            assert!(!root.join(&key).exists(), "{point:?} left the final file");
            let removed = objects.remove_local_temp_files().unwrap();
            if point == HookPoint::ObjectWrite {
                assert_eq!(removed, 0, "{point:?}");
            } else {
                assert_eq!(removed, 1, "{point:?}");
            }
            assert!(temps(&root).is_empty(), "{point:?}");
            assert!(!root.join(&key).exists());
        }
    }

    #[tokio::test]
    async fn remote_put_round_trips_and_reports_faults() {
        let inner = Arc::new(InMemory::new());
        let fault = FaultStore::new(Arc::clone(&inner) as Arc<dyn object_store::ObjectStore>);
        let objects = Objects::remote_for_test(Arc::new(fault.clone()), Hooks::default()).unwrap();
        let small = Bytes::from_static(b"small");
        objects
            .put_durable("tables/spans/a.parquet", small.clone())
            .await
            .unwrap();
        assert_eq!(
            read_registered(&objects, "tables/spans/a.parquet").await,
            small
        );

        let mut large = vec![0_u8; SINGLE_PUT_MAX + 1];
        large[0] = 1;
        large[MULTIPART_PART] = 2;
        large[SINGLE_PUT_MAX] = 3;
        let large = Bytes::from(large);
        objects
            .put_durable("tables/spans/b.parquet", large.clone())
            .await
            .unwrap();
        assert_eq!(
            read_registered(&objects, "tables/spans/b.parquet").await,
            large
        );

        fault.arm(FaultKind::FailBefore);
        let error = objects
            .put_durable("tables/spans/c.parquet", Bytes::from_static(b"x"))
            .await
            .unwrap_err();
        assert!(matches!(error, ObjectError::Upload { .. }));
        let missing = object_store::path::Path::parse("tables/spans/c.parquet").unwrap();
        assert!(matches!(
            inner.get(&missing).await.unwrap_err(),
            object_store::Error::NotFound { .. }
        ));

        fault.arm(FaultKind::FailAfter);
        let error = objects
            .put_durable("tables/spans/d.parquet", Bytes::from_static(b"landed"))
            .await
            .unwrap_err();
        assert!(matches!(error, ObjectError::Upload { .. }));
        assert_eq!(
            read_registered(&objects, "tables/spans/d.parquet").await,
            Bytes::from_static(b"landed")
        );

        fault.arm(FaultKind::Delay(Duration::from_millis(20)));
        objects
            .put_durable("tables/spans/e.parquet", Bytes::from_static(b"slow"))
            .await
            .unwrap();
        assert_eq!(
            read_registered(&objects, "tables/spans/e.parquet").await,
            Bytes::from_static(b"slow")
        );

        fault.arm(FaultKind::FailAfter);
        let error = objects
            .put_durable("tables/spans/f.parquet", large)
            .await
            .unwrap_err();
        assert!(matches!(error, ObjectError::Upload { .. }));
        let landed = read_registered(&objects, "tables/spans/f.parquet").await;
        assert_eq!(landed.len(), SINGLE_PUT_MAX + 1);
        assert_eq!(objects.remove_local_temp_files().unwrap(), 0);
        assert_eq!(OBJECT_STORE_URL, "llmobs://objects");
    }

    #[test]
    fn table_file_path_matches_the_layout_and_is_unique() {
        let (first, first_id) = Objects::table_file_path(FileTable::Spans, project(), day(), 0);
        let (second, second_id) = Objects::table_file_path(FileTable::Spans, project(), day(), 0);
        assert_ne!(first_id, second_id);
        assert_ne!(first, second);
        assert_eq!(
            first,
            format!(
                "tables/spans/project={}/day=2026-11-11/0-{first_id}.parquet",
                project()
            )
        );
        let (scores, score_id) = Objects::table_file_path(FileTable::Scores, project(), day(), 1);
        assert_eq!(
            scores,
            format!(
                "tables/scores/project={}/day=2026-11-11/1-{score_id}.parquet",
                project()
            )
        );
    }

    #[tokio::test]
    async fn rejected_keys_and_unrelated_files_stay_put() {
        let dir = tempfile::tempdir().unwrap();
        let objects = Objects::new(&local_config(dir.path()), Hooks::default())
            .await
            .unwrap();
        for key in ["", "/abs", "a/../b", "a//b", r"a\b", "a/./b"] {
            let error = objects
                .put_durable(key, Bytes::from_static(b"x"))
                .await
                .unwrap_err();
            assert!(
                matches!(error, ObjectError::InvalidPath),
                "{key:?} -> {error}"
            );
        }
        let (key, _) = Objects::table_file_path(FileTable::Spans, project(), day(), 0);
        objects
            .put_durable(&key, Bytes::from_static(b"keep"))
            .await
            .unwrap();
        let root = dir.path().join("objects");
        let parent = root.join(&key).parent().unwrap().to_path_buf();
        std::fs::write(parent.join(".keep"), b"keep").unwrap();
        std::fs::write(parent.join("note.tmp-1"), b"not-hidden").unwrap();
        assert_eq!(objects.remove_local_temp_files().unwrap(), 0);
        assert_eq!(std::fs::read(parent.join(".keep")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(parent.join("note.tmp-1")).unwrap(),
            b"not-hidden"
        );
        assert_eq!(std::fs::read(root.join(&key)).unwrap(), b"keep");
    }
}
