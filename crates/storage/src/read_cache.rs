//! The S3 read cache (spec 03 §6, D-A1-11).
//!
//! With an S3 object store, reads of data files (`tables/…`) go through
//! [`CacheStore`], which serves byte ranges from 1 MiB blocks kept under
//! `<data_dir>/cache/<file name>/`. Each block file ends with the CRC-32C of
//! its bytes; a block that fails its check, or has the wrong length, is deleted
//! and fetched again. Nothing is fsynced: a block lost or torn by a crash fails
//! that check and is fetched again. Files are immutable, so entries never go
//! stale.
//!
//! Blocks are written only while free disk is at least `2 × min_free`, and the
//! disk monitor gives blocks back while it is below `min_free` (see `disk`).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use futures::stream::BoxStream;
use object_store::path::Path as ObjectPath;
use object_store::{
    CopyOptions, GetOptions, GetRange, GetResult, GetResultPayload, ListResult, MultipartUpload,
    ObjectMeta, ObjectStore, ObjectStoreExt, PutMultipartOptions, PutOptions, PutPayload,
    PutResult,
};
use uuid::Uuid;

use crate::disk::Disk;
use crate::error::OpenError;
use crate::open_io;

/// Block size of the S3 read cache.
pub(crate) const BLOCK: u64 = 1 << 20;

/// The same block size as a `usize`, for slicing in memory.
const BLOCK_LEN: usize = 1 << 20;

/// Bytes after a block's data: its CRC-32C, little-endian.
const CRC_LEN: usize = 4;

/// The file in an object's directory that holds the object's size.
const SIZE_FILE: &str = "size";

/// Largest default capacity, in bytes (spec 03 §6).
const MAX_DEFAULT_CAPACITY: u64 = 100 << 30;

/// The default capacity for a data directory on a file system of
/// `total_disk` bytes: a tenth of it, at most 100 GiB (spec 03 §6).
pub(crate) fn default_capacity(total_disk: u64) -> u64 {
    (total_disk / 10).min(MAX_DEFAULT_CAPACITY)
}

/// Counters of the read cache, for the benchmarks (spec 03 §6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadCacheStats {
    /// Blocks served from disk.
    pub hits: u64,
    /// Blocks fetched from S3.
    pub misses: u64,
    /// Bytes of the blocks on disk, CRCs included.
    pub bytes: u64,
    /// Blocks on disk.
    pub blocks: u64,
}

/// The on-disk block cache (spec 03 §6). Cheap to share: every field is behind
/// `Arc` or a lock.
#[derive(Debug)]
pub(crate) struct ReadCache {
    dir: PathBuf,
    capacity: u64,
    disk: Arc<Disk>,
    min_free: u64,
    state: Mutex<CacheState>,
    hits: AtomicU64,
    misses: AtomicU64,
}

#[derive(Debug, Default)]
struct CacheState {
    /// Block → (last use tick, bytes on disk including the CRC).
    blocks: HashMap<(String, u64), (u64, u64)>,
    /// Last use tick → block, for eviction in LRU order.
    order: BTreeMap<u64, (String, u64)>,
    next_tick: u64,
    bytes: u64,
    /// File name → object size, for the objects whose `size` file is recorded.
    sizes: HashMap<String, u64>,
}

impl CacheState {
    fn tick(&mut self) -> u64 {
        let tick = self.next_tick;
        self.next_tick += 1;
        tick
    }

    /// Indexes block `index` of `name` as `stored` bytes on disk, used now.
    fn insert(&mut self, name: &str, index: u64, stored: u64) {
        let key = (name.to_owned(), index);
        let tick = self.tick();
        self.blocks.insert(key.clone(), (tick, stored));
        self.order.insert(tick, key);
        self.bytes = self.bytes.saturating_add(stored);
    }

    /// Marks an indexed block as used now.
    fn touch(&mut self, key: &(String, u64)) {
        let tick = self.tick();
        if let Some((previous, _)) = self.blocks.get_mut(key) {
            let old = *previous;
            *previous = tick;
            self.order.remove(&old);
            self.order.insert(tick, key.clone());
        }
    }

    /// Drops block `index` of `name` from the index. Returns its stored bytes.
    fn forget(&mut self, name: &str, index: u64) -> Option<u64> {
        let (tick, stored) = self.blocks.remove(&(name.to_owned(), index))?;
        self.order.remove(&tick);
        self.bytes = self.bytes.saturating_sub(stored);
        Some(stored)
    }

    /// Drops the least recently used block from the index.
    fn pop_lru(&mut self) -> Option<((String, u64), u64)> {
        let (_, key) = self.order.pop_first()?;
        let stored = self.blocks.remove(&key).map_or(0, |(_, stored)| stored);
        self.bytes = self.bytes.saturating_sub(stored);
        Some((key, stored))
    }
}

impl ReadCache {
    /// Creates `dir`, deletes temp files, and rebuilds the index from the
    /// files: block files are checked by name and length, not by CRC; the
    /// oldest by modification time are evicted down to `capacity`. A directory
    /// whose `size` file does not parse is deleted. Blocking: call from
    /// `spawn_blocking`.
    pub(crate) fn open(
        dir: PathBuf,
        capacity: u64,
        disk: Arc<Disk>,
        min_free: u64,
    ) -> Result<Self, OpenError> {
        fs::create_dir_all(&dir).map_err(|source| open_io("creating read cache", &dir, source))?;
        let mut state = CacheState::default();
        let mut found: Vec<(SystemTime, String, u64, u64)> = Vec::new();
        let entries =
            fs::read_dir(&dir).map_err(|source| open_io("reading read cache", &dir, source))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let size = fs::read_to_string(path.join(SIZE_FILE))
                .ok()
                .and_then(|text| text.parse::<u64>().ok());
            let Some(size) = size else {
                // Without its size the blocks cannot be checked: drop them all.
                remove_dir(&path);
                continue;
            };
            let Ok(files) = fs::read_dir(&path) else {
                remove_dir(&path);
                continue;
            };
            for file in files.flatten() {
                let file_path = file.path();
                let Ok(file_name) = file.file_name().into_string() else {
                    remove_file(&file_path);
                    continue;
                };
                if is_temp_name(&file_name) {
                    remove_file(&file_path);
                    continue;
                }
                if file_name == SIZE_FILE {
                    continue;
                }
                let index = match file_name.parse::<u64>() {
                    Ok(index) if index < block_count(size) => index,
                    _ => {
                        remove_file(&file_path);
                        continue;
                    }
                };
                match file.metadata() {
                    Ok(meta) => {
                        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
                        found.push((modified, name.clone(), index, meta.len()));
                    }
                    Err(_) => remove_file(&file_path),
                }
            }
            state.sizes.insert(name, size);
        }
        found.sort();
        for (_, name, index, stored) in found {
            state.insert(&name, index, stored);
        }
        let cache = Self {
            dir,
            capacity,
            disk,
            min_free,
            state: Mutex::new(state),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        };
        let victims = {
            let mut state = cache.lock();
            cache.evict_over_capacity(&mut state)
        };
        cache.remove_files(victims);
        Ok(cache)
    }

    fn lock(&self) -> MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether blocks may be written: free disk is at least `2 × min_free`.
    fn writable(&self) -> bool {
        self.disk.free() >= self.min_free.saturating_mul(2)
    }

    /// The object's size, when it is recorded.
    pub(crate) fn size(&self, name: &str) -> Option<u64> {
        self.lock().sizes.get(name).copied()
    }

    /// Records the object's size and writes its `size` file. Nothing is written
    /// while free disk is under `2 × min_free`; the size is then not recorded.
    /// Blocking.
    pub(crate) fn set_size(&self, name: &str, size: u64) -> io::Result<()> {
        if self.size(name).is_some() || !self.writable() {
            return Ok(());
        }
        let dir = self.dir.join(name);
        fs::create_dir_all(&dir)?;
        write_renamed(&dir, SIZE_FILE, size.to_string().as_bytes())?;
        self.lock().sizes.insert(name.to_owned(), size);
        Ok(())
    }

    /// The blocks of `name` among `indexes` that are cached. A block whose file
    /// is missing, has the wrong length or fails its CRC is removed from the
    /// index and its file deleted, and is left out. Found blocks are marked used.
    /// Blocking.
    pub(crate) fn read_blocks(&self, name: &str, indexes: &[u64]) -> Vec<(u64, Bytes)> {
        let (size, present) = {
            let state = self.lock();
            let Some(size) = state.sizes.get(name).copied() else {
                return Vec::new();
            };
            let present: Vec<u64> = indexes
                .iter()
                .copied()
                .filter(|index| state.blocks.contains_key(&(name.to_owned(), *index)))
                .collect();
            (size, present)
        };
        let mut good = Vec::with_capacity(present.len());
        let mut bad = Vec::new();
        for index in present {
            match read_block(&self.dir, name, index, size) {
                Some(data) => good.push((index, data)),
                None => bad.push(index),
            }
        }
        {
            let mut state = self.lock();
            for (index, _) in &good {
                state.touch(&(name.to_owned(), *index));
            }
            for index in &bad {
                state.forget(name, *index);
            }
        }
        for index in &bad {
            remove_block_file(&self.dir, name, *index);
        }
        self.hits.fetch_add(len_u64(good.len()), Ordering::Relaxed);
        good
    }

    /// Writes `blocks` of `name`, the bytes of each block without its CRC. Blocks
    /// already cached are skipped. Nothing is written while free disk is under
    /// `2 × min_free`, nor for an object whose size is not recorded. Evicts the
    /// least recently used blocks while the cache is over capacity. Failures are
    /// logged at debug and skipped. Blocking.
    pub(crate) fn write_blocks(&self, name: &str, blocks: &[(u64, Bytes)]) {
        if !self.writable() {
            return;
        }
        let Some(size) = self.size(name) else {
            return;
        };
        let dir = self.dir.join(name);
        for (index, data) in blocks {
            let index = *index;
            if block_len(size, index) != Some(len_u64(data.len())) {
                tracing::debug!(index, "read cache block has the wrong length; not cached");
                continue;
            }
            let key = (name.to_owned(), index);
            if self.lock().blocks.contains_key(&key) {
                continue;
            }
            let mut file = Vec::with_capacity(data.len() + CRC_LEN);
            file.extend_from_slice(data);
            file.extend_from_slice(&crc32c::crc32c(data).to_le_bytes());
            if let Err(error) = fs::create_dir_all(&dir)
                .and_then(|()| write_renamed(&dir, &index.to_string(), &file))
            {
                tracing::debug!(%error, index, "writing a read cache block failed");
                continue;
            }
            let victims = {
                let mut state = self.lock();
                if !state.sizes.contains_key(name) {
                    // The object was evicted while this block was written.
                    drop(state);
                    remove_block_file(&self.dir, name, index);
                    continue;
                }
                if !state.blocks.contains_key(&key) {
                    state.insert(name, index, len_u64(file.len()));
                }
                self.evict_over_capacity(&mut state)
            };
            self.remove_files(victims);
        }
    }

    /// Removes every block of `name`, its size, and its directory. Blocking.
    pub(crate) fn evict_file(&self, name: &str) {
        {
            let mut state = self.lock();
            if let Some(size) = state.sizes.remove(name) {
                for index in 0..block_count(size) {
                    state.forget(name, index);
                }
            }
        }
        remove_dir(&self.dir.join(name));
    }

    /// Evicts least recently used blocks until at least `bytes` are freed.
    /// Blocking.
    pub(crate) fn shed(&self, bytes: u64) {
        let victims = {
            let mut state = self.lock();
            let mut freed = 0_u64;
            let mut victims = Vec::new();
            while freed < bytes {
                let Some((key, stored)) = state.pop_lru() else {
                    break;
                };
                freed = freed.saturating_add(stored);
                victims.push(key);
            }
            victims
        };
        self.remove_files(victims);
    }

    /// Write-through for a data file just uploaded: its size and every block.
    /// Blocking.
    pub(crate) fn insert_bytes(&self, name: &str, bytes: &Bytes) {
        let Ok(size) = u64::try_from(bytes.len()) else {
            return;
        };
        if let Err(error) = self.set_size(name, size) {
            tracing::debug!(%error, "recording a read cache size failed");
            return;
        }
        let blocks: Vec<(u64, Bytes)> = (0..block_count(size))
            .filter_map(|index| {
                let start = usize::try_from(index.checked_mul(BLOCK)?).ok()?;
                let end = bytes.len().min(start.saturating_add(BLOCK_LEN));
                Some((index, bytes.slice(start..end)))
            })
            .collect();
        self.write_blocks(name, &blocks);
    }

    /// The same as [`Self::insert_bytes`] for a local file of `size` bytes, read
    /// one block at a time. Blocking.
    pub(crate) fn insert_file(&self, name: &str, file: &mut fs::File, size: u64) {
        if !self.writable() {
            return;
        }
        if let Err(error) = self.set_size(name, size) {
            tracing::debug!(%error, "recording a read cache size failed");
            return;
        }
        for index in 0..block_count(size) {
            let Some(len) = block_len(size, index).and_then(|len| usize::try_from(len).ok()) else {
                return;
            };
            let mut buffer = vec![0_u8; len];
            if let Err(error) = file.read_exact(&mut buffer) {
                tracing::debug!(%error, "reading a file for the read cache failed");
                return;
            }
            self.write_blocks(name, &[(index, Bytes::from(buffer))]);
        }
    }

    pub(crate) fn stats(&self) -> ReadCacheStats {
        let state = self.lock();
        ReadCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            bytes: state.bytes,
            blocks: len_u64(state.blocks.len()),
        }
    }

    /// Pops least recently used blocks while the cache is over capacity. Returns
    /// their keys; the caller deletes the files outside the lock.
    fn evict_over_capacity(&self, state: &mut CacheState) -> Vec<(String, u64)> {
        let mut victims = Vec::new();
        while state.bytes > self.capacity {
            let Some((key, _)) = state.pop_lru() else {
                break;
            };
            victims.push(key);
        }
        victims
    }

    fn remove_files(&self, victims: Vec<(String, u64)>) {
        for (name, index) in victims {
            remove_block_file(&self.dir, &name, index);
        }
    }
}

/// The store that serves data-file reads from a [`ReadCache`] (spec 03 §6).
/// Media, writes, listings and copies pass through to `inner`.
#[derive(Debug)]
pub(crate) struct CacheStore {
    pub(crate) inner: Arc<dyn ObjectStore>,
    pub(crate) cache: Arc<ReadCache>,
}

impl std::fmt::Display for CacheStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ReadCache({})", self.inner.as_ref())
    }
}

impl CacheStore {
    /// The object's size, from the cache or one HEAD, which is then recorded.
    async fn size_of(&self, location: &ObjectPath, name: &str) -> object_store::Result<u64> {
        if let Some(size) = self.cache.size(name) {
            return Ok(size);
        }
        let size = self.inner.head(location).await?.size;
        let cache = Arc::clone(&self.cache);
        let owned = name.to_owned();
        match tokio::task::spawn_blocking(move || cache.set_size(&owned, size)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::debug!(%error, "recording a read cache size failed"),
            Err(error) => tracing::debug!(%error, "recording a read cache size failed"),
        }
        Ok(size)
    }

    /// The object's size and one `Bytes` per range, in order. Ranges are clamped
    /// to the size. A range that starts at or after the end is the store's error.
    async fn read(
        &self,
        location: &ObjectPath,
        name: &str,
        ranges: &[Range<u64>],
    ) -> object_store::Result<(u64, Vec<Bytes>)> {
        let size = self.size_of(location, name).await?;
        if ranges
            .iter()
            .any(|range| range.start >= range.end || range.start >= size)
        {
            return self
                .inner
                .get_ranges(location, ranges)
                .await
                .map(|parts| (size, parts));
        }
        let clamped: Vec<Range<u64>> = ranges
            .iter()
            .map(|range| range.start..range.end.min(size))
            .collect();
        let mut indexes: Vec<u64> = clamped
            .iter()
            .flat_map(|range| (range.start / BLOCK)..=((range.end - 1) / BLOCK))
            .collect();
        indexes.sort_unstable();
        indexes.dedup();

        let cache = Arc::clone(&self.cache);
        let owned = name.to_owned();
        let wanted = indexes.clone();
        let found = tokio::task::spawn_blocking(move || cache.read_blocks(&owned, &wanted))
            .await
            .unwrap_or_default();
        let mut blocks: BTreeMap<u64, Bytes> = found.into_iter().collect();

        let missing: Vec<u64> = indexes
            .iter()
            .copied()
            .filter(|index| !blocks.contains_key(index))
            .collect();
        if !missing.is_empty() {
            let runs = runs(&missing);
            let byte_ranges: Vec<Range<u64>> = runs
                .iter()
                .map(|&(first, last)| (first * BLOCK)..((last + 1) * BLOCK).min(size))
                .collect();
            let parts = self.inner.get_ranges(location, &byte_ranges).await?;
            let mut fetched: Vec<(u64, Bytes)> = Vec::with_capacity(missing.len());
            for (&(first, last), part) in runs.iter().zip(parts) {
                for index in first..=last {
                    let expected = block_len(size, index).ok_or_else(short_read)?;
                    let data = block_in(&part, (index - first) * BLOCK, expected)?;
                    fetched.push((index, data));
                }
            }
            self.cache
                .misses
                .fetch_add(len_u64(fetched.len()), Ordering::Relaxed);
            let cache = Arc::clone(&self.cache);
            let owned = name.to_owned();
            let to_write = fetched.clone();
            // Best effort: a block that is not written is read from S3 again.
            let _ =
                tokio::task::spawn_blocking(move || cache.write_blocks(&owned, &to_write)).await;
            blocks.extend(fetched);
        }

        let mut parts = Vec::with_capacity(clamped.len());
        for range in &clamped {
            parts.push(assemble(range, &blocks)?);
        }
        Ok((size, parts))
    }
}

#[async_trait]
impl ObjectStore for CacheStore {
    async fn put_opts(
        &self,
        location: &ObjectPath,
        payload: PutPayload,
        opts: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &ObjectPath,
        opts: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(location, opts).await
    }

    async fn get_opts(
        &self,
        location: &ObjectPath,
        options: GetOptions,
    ) -> object_store::Result<GetResult> {
        let bounded = match &options.range {
            Some(GetRange::Bounded(range)) => Some(range.clone()),
            _ => None,
        };
        let plain = !options.head
            && options.if_match.is_none()
            && options.if_none_match.is_none()
            && options.if_modified_since.is_none()
            && options.if_unmodified_since.is_none()
            && options.version.is_none();
        match (cached_name(location), bounded, plain) {
            (Some(name), Some(range), true) => {
                let (size, mut parts) = self
                    .read(location, &name, std::slice::from_ref(&range))
                    .await?;
                let bytes = parts.pop().ok_or_else(short_read)?;
                Ok(GetResult {
                    payload: GetResultPayload::Stream(
                        futures::stream::once(
                            async move { Ok::<Bytes, object_store::Error>(bytes) },
                        )
                        .boxed(),
                    ),
                    meta: ObjectMeta {
                        location: location.clone(),
                        // Data files are immutable; the cache keeps no modification time.
                        last_modified: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
                        size,
                        e_tag: None,
                        version: None,
                    },
                    range: range.start..range.end.min(size),
                    attributes: object_store::Attributes::default(),
                })
            }
            _ => self.inner.get_opts(location, options).await,
        }
    }

    async fn get_ranges(
        &self,
        location: &ObjectPath,
        ranges: &[Range<u64>],
    ) -> object_store::Result<Vec<Bytes>> {
        match cached_name(location) {
            Some(name) => self
                .read(location, &name, ranges)
                .await
                .map(|(_, parts)| parts),
            None => self.inner.get_ranges(location, ranges).await,
        }
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, object_store::Result<ObjectPath>>,
    ) -> BoxStream<'static, object_store::Result<ObjectPath>> {
        let cache = Arc::clone(&self.cache);
        self.inner
            .delete_stream(locations)
            .then(move |result| {
                let cache = Arc::clone(&cache);
                async move {
                    if let Ok(path) = &result
                        && let Some(name) = cached_name(path)
                    {
                        let _ = tokio::task::spawn_blocking(move || cache.evict_file(&name)).await;
                    }
                    result
                }
            })
            .boxed()
    }

    fn list(
        &self,
        prefix: Option<&ObjectPath>,
    ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        self.inner.list(prefix)
    }

    async fn list_with_delimiter(
        &self,
        prefix: Option<&ObjectPath>,
    ) -> object_store::Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(
        &self,
        from: &ObjectPath,
        to: &ObjectPath,
        options: CopyOptions,
    ) -> object_store::Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
}

/// The cache name of `location` when it is a data file (`tables/…`): its last
/// segment, which is unique (UUIDv7, spec 03 § Layout). `None` for anything else.
pub(crate) fn cached_name(location: &ObjectPath) -> Option<String> {
    let mut parts = location.parts();
    let first = parts.next()?;
    parts.next()?;
    if first.as_ref() != "tables" {
        return None;
    }
    location.filename().map(str::to_owned)
}

/// Blocks in the object of `size` bytes: `ceil(size / BLOCK)`.
fn block_count(size: u64) -> u64 {
    size.div_ceil(BLOCK)
}

/// The data length of block `index` of an object of `size` bytes, or `None`
/// when the block lies past the end.
fn block_len(size: u64, index: u64) -> Option<u64> {
    let start = index.checked_mul(BLOCK)?;
    if start >= size {
        return None;
    }
    Some((size - start).min(BLOCK))
}

/// The data of a block file, or `None` when it is shorter than a CRC or its
/// CRC does not match.
fn block_data(file: &[u8]) -> Option<&[u8]> {
    let split = file.len().checked_sub(CRC_LEN)?;
    let (data, crc) = file.split_at(split);
    let stored = u32::from_le_bytes(crc.try_into().ok()?);
    (crc32c::crc32c(data) == stored).then_some(data)
}

/// Reads and checks block `index` of `name`. `None` when the file is missing,
/// has the wrong length, or fails its CRC.
fn read_block(dir: &Path, name: &str, index: u64, size: u64) -> Option<Bytes> {
    let expected = block_len(size, index)?;
    let file = Bytes::from(fs::read(dir.join(name).join(index.to_string())).ok()?);
    let data = block_data(&file)?;
    (len_u64(data.len()) == expected).then(|| file.slice(..data.len()))
}

/// Groups sorted indexes into inclusive `(first, last)` runs of consecutive values.
fn runs(sorted: &[u64]) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for &index in sorted {
        match runs.last_mut() {
            Some((_, last)) if *last + 1 == index => *last = index,
            _ => runs.push((index, index)),
        }
    }
    runs
}

/// The `expected` bytes at `offset` of a fetched run. A short run is an error.
fn block_in(part: &Bytes, offset: u64, expected: u64) -> object_store::Result<Bytes> {
    let start = usize::try_from(offset).map_err(|_| short_read())?;
    let end = offset
        .checked_add(expected)
        .and_then(|end| usize::try_from(end).ok())
        .ok_or_else(short_read)?;
    if end > part.len() {
        return Err(short_read());
    }
    Ok(part.slice(start..end))
}

/// The bytes of `range` from the blocks it touches. Every touched block must be
/// in `blocks`.
fn assemble(range: &Range<u64>, blocks: &BTreeMap<u64, Bytes>) -> object_store::Result<Bytes> {
    let first = range.start / BLOCK;
    let last = (range.end - 1) / BLOCK;
    if first == last {
        let block = blocks.get(&first).ok_or_else(short_read)?;
        let base = first * BLOCK;
        return slice_of(block, range.start - base, range.end - base);
    }
    let capacity = usize::try_from(range.end - range.start).unwrap_or(0);
    let mut out = BytesMut::with_capacity(capacity);
    for index in first..=last {
        let block = blocks.get(&index).ok_or_else(short_read)?;
        let base = index * BLOCK;
        let from = range.start.max(base) - base;
        let to = range.end.min(base.saturating_add(len_u64(block.len()))) - base;
        out.extend_from_slice(&slice_of(block, from, to)?);
    }
    Ok(out.freeze())
}

fn slice_of(block: &Bytes, from: u64, to: u64) -> object_store::Result<Bytes> {
    let from = usize::try_from(from).map_err(|_| short_read())?;
    let to = usize::try_from(to).map_err(|_| short_read())?;
    if from > to || to > block.len() {
        return Err(short_read());
    }
    Ok(block.slice(from..to))
}

/// The error for a cached block that is missing or short. It can only follow
/// from a bug, since blocks are checked before use.
fn short_read() -> object_store::Error {
    object_store::Error::Generic {
        store: "ReadCache",
        source: "a read cache block is missing or short".into(),
    }
}

fn len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// `<name>.tmp-…` in a cache directory, as the write path names them.
fn is_temp_name(name: &str) -> bool {
    name.starts_with('.') && name.contains(".tmp-")
}

/// Writes `bytes` to `dir/name` through a temp file renamed into place.
fn write_renamed(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temp = dir.join(format!(".{name}.tmp-{}", Uuid::now_v7()));
    if let Err(error) = fs::write(&temp, bytes) {
        remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp, dir.join(name)) {
        remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn remove_block_file(dir: &Path, name: &str, index: u64) {
    remove_file(&dir.join(name).join(index.to_string()));
}

/// Deletes a file; a missing file is fine, any other error is logged.
fn remove_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::debug!(%error, "removing a read cache file failed"),
    }
}

/// Deletes a directory tree; a missing directory is fine, any other error is logged.
fn remove_dir(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::debug!(%error, "removing a read cache directory failed"),
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use bytes::Bytes;
    use object_store::memory::InMemory;
    use object_store::path::Path as ObjectPath;
    use object_store::{GetOptions, GetRange, ObjectStore, ObjectStoreExt};

    use super::*;
    use crate::test_support::FaultStore;

    const MIB: u64 = 1 << 20;
    const DATA: &str =
        "tables/spans/project=p/day=2026-11-11/0-0192e5a0-7c1a-7000-8000-000000000001.parquet";
    const NAME: &str = "0-0192e5a0-7c1a-7000-8000-000000000001.parquet";

    /// Deterministic content: byte `i` is `i % 251`.
    fn content(len: u64) -> Bytes {
        (0..len)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect::<Vec<u8>>()
            .into()
    }

    fn expected(len: u64, range: Range<u64>) -> Bytes {
        content(len)
            .slice(usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap())
    }

    /// A cache over `inner` in `dir`, with its disk reporting `free` bytes.
    struct Fixture {
        inner: Arc<InMemory>,
        fault: FaultStore,
        cache: Arc<ReadCache>,
        store: CacheStore,
        cache_dir: PathBuf,
    }

    fn fixture(dir: &Path, capacity: u64, min_free: u64) -> Fixture {
        let inner = Arc::new(InMemory::new());
        let fault = FaultStore::new(Arc::clone(&inner) as Arc<dyn ObjectStore>);
        let disk = Arc::new(Disk::open(vec![dir.to_path_buf()], 0).unwrap());
        let cache_dir = dir.join("cache");
        let cache = Arc::new(ReadCache::open(cache_dir.clone(), capacity, disk, min_free).unwrap());
        let store = CacheStore {
            inner: Arc::new(fault.clone()),
            cache: Arc::clone(&cache),
        };
        Fixture {
            inner,
            fault,
            cache,
            store,
            cache_dir,
        }
    }

    async fn put(inner: &InMemory, path: &str, bytes: Bytes) {
        inner
            .put(&ObjectPath::parse(path).unwrap(), bytes.into())
            .await
            .unwrap();
    }

    async fn read(
        store: &CacheStore,
        path: &str,
        range: Range<u64>,
    ) -> object_store::Result<Bytes> {
        let options = GetOptions {
            range: Some(GetRange::Bounded(range)),
            ..GetOptions::default()
        };
        store
            .get_opts(&ObjectPath::parse(path).unwrap(), options)
            .await?
            .bytes()
            .await
    }

    fn block_file(cache_dir: &Path, index: u64) -> PathBuf {
        cache_dir.join(NAME).join(index.to_string())
    }

    #[tokio::test]
    async fn a_range_is_served_from_blocks_after_the_first_read() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(7 * MIB / 2)).await;
        let range = (MIB - 10)..(2 * MIB + 10);

        let first = read(&fixture.store, DATA, range.clone()).await.unwrap();
        assert_eq!(first, expected(7 * MIB / 2, range.clone()));
        assert_eq!(fixture.cache.stats().misses, 3);

        let gets = fixture.fault.gets();
        let second = read(&fixture.store, DATA, range.clone()).await.unwrap();
        assert_eq!(second, expected(7 * MIB / 2, range));
        assert_eq!(fixture.fault.gets(), gets, "the second read went to S3");
        assert_eq!(fixture.cache.stats().hits, 3);
        assert_eq!(fixture.cache.stats().misses, 3);
    }

    #[tokio::test]
    async fn the_last_block_is_short_and_ranges_past_the_end_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(7 * MIB / 2)).await;
        let options = GetOptions {
            range: Some(GetRange::Bounded(3 * MIB..4 * MIB)),
            ..GetOptions::default()
        };
        let result = fixture
            .store
            .get_opts(&ObjectPath::parse(DATA).unwrap(), options)
            .await
            .unwrap();
        assert_eq!(result.range, (3 * MIB)..(7 * MIB / 2));
        assert_eq!(result.meta.size, 7 * MIB / 2);
        let bytes = result.bytes().await.unwrap();
        assert_eq!(bytes.len() as u64, MIB / 2);
        assert_eq!(bytes, expected(7 * MIB / 2, (3 * MIB)..(7 * MIB / 2)));
    }

    #[tokio::test]
    async fn a_start_past_the_end_is_the_stores_error() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(7 * MIB / 2)).await;
        let location = ObjectPath::parse(DATA).unwrap();
        let direct = fixture
            .inner
            .get_ranges(&location, std::slice::from_ref(&((4 * MIB)..(5 * MIB))))
            .await
            .unwrap_err();
        let through = read(&fixture.store, DATA, (4 * MIB)..(5 * MIB))
            .await
            .unwrap_err();
        assert_eq!(through.to_string(), direct.to_string());
    }

    #[tokio::test]
    async fn media_and_whole_reads_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        let media = "media/p/0123456789abcdef";
        put(&fixture.inner, media, content(64)).await;
        put(&fixture.inner, DATA, content(MIB + 5)).await;

        assert_eq!(
            read(&fixture.store, media, 2..9).await.unwrap(),
            expected(64, 2..9)
        );
        let whole = fixture
            .store
            .get_opts(&ObjectPath::parse(DATA).unwrap(), GetOptions::default())
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(whole, content(MIB + 5));
        assert_eq!(fixture.cache.stats().blocks, 0);
        assert_eq!(fs::read_dir(&fixture.cache_dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_corrupt_block_is_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(2 * MIB)).await;
        assert_eq!(
            read(&fixture.store, DATA, 0..10).await.unwrap(),
            expected(2 * MIB, 0..10)
        );

        let path = block_file(&fixture.cache_dir, 0);
        let mut file = fs::read(&path).unwrap();
        file[5] ^= 0xFF;
        fs::write(&path, &file).unwrap();

        assert_eq!(
            read(&fixture.store, DATA, 0..10).await.unwrap(),
            expected(2 * MIB, 0..10)
        );
        assert_eq!(fixture.cache.stats().misses, 2);
        assert!(block_data(&fs::read(&path).unwrap()).is_some());
    }

    #[tokio::test]
    async fn eviction_keeps_the_cache_under_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let capacity = 2 * (MIB + CRC_LEN as u64);
        let fixture = fixture(dir.path(), capacity, 0);
        put(&fixture.inner, DATA, content(7 * MIB / 2)).await;
        read(&fixture.store, DATA, 0..(3 * MIB)).await.unwrap();
        assert_eq!(fixture.cache.stats().blocks, 2);

        assert_eq!(
            read(&fixture.store, DATA, 0..10).await.unwrap(),
            expected(7 * MIB / 2, 0..10)
        );
        let stats = fixture.cache.stats();
        assert_eq!(stats.misses, 4, "block 0 was evicted");
        assert_eq!(stats.blocks, 2);
        assert!(stats.bytes <= capacity);
    }

    #[tokio::test]
    async fn the_index_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(7 * MIB / 2)).await;
        read(&fixture.store, DATA, 0..(3 * MIB)).await.unwrap();
        let cache_dir = fixture.cache_dir.clone();
        let inner = Arc::clone(&fixture.inner);
        drop(fixture);

        let disk = Arc::new(Disk::open(vec![dir.path().to_path_buf()], 0).unwrap());
        let cache = Arc::new(ReadCache::open(cache_dir, 64 * MIB, disk, 0).unwrap());
        assert_eq!(cache.stats().blocks, 3);
        let store = CacheStore {
            inner: Arc::new(FaultStore::new(inner as Arc<dyn ObjectStore>)),
            cache: Arc::clone(&cache),
        };
        assert_eq!(
            read(&store, DATA, 0..(3 * MIB)).await.unwrap(),
            expected(7 * MIB / 2, 0..(3 * MIB))
        );
        let stats = cache.stats();
        assert_eq!((stats.hits, stats.misses), (3, 0));
    }

    #[tokio::test]
    async fn a_size_file_that_does_not_parse_drops_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(cache_dir.join(NAME)).unwrap();
        fs::write(cache_dir.join(NAME).join(SIZE_FILE), "12x").unwrap();
        fs::write(cache_dir.join(NAME).join("0"), b"stray").unwrap();
        let disk = Arc::new(Disk::open(vec![dir.path().to_path_buf()], 0).unwrap());
        let cache = ReadCache::open(cache_dir.clone(), 64 * MIB, disk, 0).unwrap();
        assert_eq!(cache.stats().blocks, 0);
        assert!(!cache_dir.join(NAME).exists());
    }

    #[tokio::test]
    async fn a_delete_evicts_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(2 * MIB)).await;
        read(&fixture.store, DATA, 0..(2 * MIB)).await.unwrap();
        assert!(fixture.cache_dir.join(NAME).is_dir());

        fixture
            .store
            .delete(&ObjectPath::parse(DATA).unwrap())
            .await
            .unwrap();
        assert!(!fixture.cache_dir.join(NAME).exists());
        assert_eq!(fixture.cache.stats().blocks, 0);
        assert_eq!(fixture.cache.stats().bytes, 0);
    }

    #[tokio::test]
    async fn nothing_is_written_while_the_disk_is_low() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(InMemory::new());
        put(&inner, DATA, content(2 * MIB)).await;
        let disk = Arc::new(Disk::open(vec![dir.path().to_path_buf()], MIB).unwrap());
        disk.set_free_for_test(Some(MIB));
        let cache_dir = dir.path().join("cache");
        let cache = Arc::new(ReadCache::open(cache_dir.clone(), 64 * MIB, disk, MIB).unwrap());
        let store = CacheStore {
            inner: inner as Arc<dyn ObjectStore>,
            cache: Arc::clone(&cache),
        };
        assert_eq!(
            read(&store, DATA, 0..(2 * MIB)).await.unwrap(),
            expected(2 * MIB, 0..(2 * MIB))
        );
        assert_eq!(cache.stats().blocks, 0);
        assert!(!cache_dir.join(NAME).exists());
    }

    #[tokio::test]
    async fn shed_frees_the_requested_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture(dir.path(), 64 * MIB, 0);
        put(&fixture.inner, DATA, content(7 * MIB / 2)).await;
        read(&fixture.store, DATA, 0..(3 * MIB)).await.unwrap();
        let before = fixture.cache.stats().bytes;
        fixture.cache.shed(MIB);
        let after = fixture.cache.stats();
        assert_eq!(after.blocks, 2);
        assert!(before - after.bytes >= MIB);
    }

    #[test]
    fn default_capacity_is_a_tenth_of_the_disk_at_most_100_gib() {
        assert_eq!(default_capacity(50 << 30), 5 << 30);
        assert_eq!(default_capacity(10 << 40), 100 << 30);
        assert_eq!(default_capacity(0), 0);
    }

    #[test]
    fn cached_names_are_the_data_files() {
        assert_eq!(
            cached_name(&ObjectPath::parse(DATA).unwrap()).as_deref(),
            Some(NAME)
        );
        assert_eq!(
            cached_name(&ObjectPath::parse("media/p/abc").unwrap()),
            None
        );
        assert_eq!(cached_name(&ObjectPath::parse("tables").unwrap()), None);
    }

    #[test]
    fn runs_group_consecutive_indexes() {
        assert_eq!(runs(&[1, 2, 3, 7, 9, 10]), vec![(1, 3), (7, 7), (9, 10)]);
        assert!(runs(&[]).is_empty());
    }

    #[test]
    fn block_lengths_cover_the_object() {
        assert_eq!(block_count(0), 0);
        assert_eq!(block_count(7 * MIB / 2), 4);
        assert_eq!(block_len(7 * MIB / 2, 3), Some(MIB / 2));
        assert_eq!(block_len(7 * MIB / 2, 4), None);
        assert_eq!(block_len(MIB, 0), Some(MIB));
    }
}
