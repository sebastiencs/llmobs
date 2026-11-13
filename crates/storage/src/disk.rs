//! Free disk and the bytes compaction jobs reserve against it (spec 03 §10,
//! D-A1-9).
//!
//! The monitor measures every second. Under `storage.min_free_bytes` the
//! appends and media puts are refused; flushes, compaction, GC and deletions
//! keep running, because they reclaim space. Each compaction job reserves its
//! input size first and is deferred when it doesn't fit.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::error::OpenError;
use crate::open_io;

/// How often the monitor measures free disk.
const INTERVAL: Duration = Duration::from_secs(1);

/// Free disk, refreshed by the monitor, and the bytes compaction reserved.
#[derive(Debug)]
pub(crate) struct Disk {
    /// `data_dir`, plus the object store root in local mode.
    paths: Vec<PathBuf>,
    min_free: u64,
    free: AtomicU64,
    /// Bytes held by running compaction jobs.
    reserved: Mutex<u64>,
    /// Tests set this to stand in for `statvfs`.
    #[cfg(test)]
    fake_free: Mutex<Option<u64>>,
}

impl Disk {
    /// Measures once. A `statvfs` error is an [`OpenError::Io`].
    pub(crate) fn open(paths: Vec<PathBuf>, min_free: u64) -> Result<Self, OpenError> {
        let disk = Self {
            paths,
            min_free,
            free: AtomicU64::new(0),
            reserved: Mutex::new(0),
            #[cfg(test)]
            fake_free: Mutex::new(None),
        };
        let free = disk
            .measure()
            .map_err(|(path, error)| open_io("measuring free disk", path, error))?;
        disk.free.store(free, Ordering::Release);
        Ok(disk)
    }

    /// Measures again and stores the result. Blocking: callers use `spawn_blocking`.
    pub(crate) fn refresh(&self) -> io::Result<u64> {
        let free = self.measure().map_err(|(_, error)| error)?;
        self.free.store(free, Ordering::Release);
        Ok(free)
    }

    /// The last measured free bytes.
    pub(crate) fn free(&self) -> u64 {
        self.free.load(Ordering::Acquire)
    }

    /// Whether ingestion must be refused: `free() < min_free`.
    pub(crate) fn low(&self) -> bool {
        self.free() < self.min_free
    }

    /// Reserves `bytes` when `free() - reserved >= bytes` (saturating). The
    /// reservation is given back when dropped.
    pub(crate) fn try_reserve(self: &Arc<Self>, bytes: u64) -> Option<DiskReservation> {
        let mut reserved = self.reserved.lock().unwrap_or_else(PoisonError::into_inner);
        if self.free().saturating_sub(*reserved) < bytes {
            return None;
        }
        *reserved = reserved.saturating_add(bytes);
        Some(DiskReservation {
            disk: Arc::clone(self),
            bytes,
        })
    }

    /// The least free space of the paths, or the first path that failed.
    fn measure(&self) -> Result<u64, (&Path, io::Error)> {
        #[cfg(test)]
        if let Some(free) = *self
            .fake_free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Ok(free);
        }
        let mut least = u64::MAX;
        for path in &self.paths {
            let free = free_bytes(path).map_err(|error| (path.as_path(), error))?;
            least = least.min(free);
        }
        Ok(least)
    }

    /// Sets the free bytes `measure` reports (`None` measures the disk again),
    /// and refreshes the stored value at once.
    #[cfg(test)]
    pub(crate) fn set_free_for_test(&self, free: Option<u64>) {
        *self
            .fake_free
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = free;
        let _ = self.refresh();
    }
}

/// Bytes a compaction job holds against free disk.
#[derive(Debug)]
pub(crate) struct DiskReservation {
    disk: Arc<Disk>,
    bytes: u64,
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        let mut reserved = self
            .disk
            .reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *reserved = reserved.saturating_sub(self.bytes);
    }
}

/// Free bytes of the file system holding `path`: `f_bavail * f_frsize`.
fn free_bytes(path: &Path) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// A running monitor task. Drop cancels it.
#[derive(Debug)]
pub(crate) struct DiskHandle {
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl DiskHandle {
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
}

/// Refreshes `disk` every second until stopped. The caller measured once at open.
pub(crate) fn spawn(disk: Arc<Disk>) -> DiskHandle {
    let (stop_tx, mut stop) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
        let mut measuring_ok = true;
        let mut was_low = disk.low();
        loop {
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let measured = Arc::clone(&disk);
                    let outcome = match tokio::task::spawn_blocking(move || measured.refresh()).await {
                        Ok(result) => result,
                        Err(_) => Err(io::Error::other("measuring task panicked")),
                    };
                    match outcome {
                        Ok(_) => measuring_ok = true,
                        Err(error) => {
                            if measuring_ok {
                                tracing::warn!(%error, "measuring free disk failed");
                            }
                            measuring_ok = false;
                        }
                    }
                    let now_low = disk.low();
                    if now_low != was_low {
                        let free = disk.free();
                        let min_free = disk.min_free;
                        if now_low {
                            tracing::warn!(
                                free,
                                min_free,
                                "free disk is under storage.min_free_bytes; ingestion is refused"
                            );
                        } else {
                            tracing::info!(free, "free disk is back above storage.min_free_bytes");
                        }
                        was_low = now_low;
                    }
                }
            }
        }
    });
    DiskHandle {
        stop: stop_tx,
        task: Mutex::new(Some(task)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_reserve_grants_up_to_free_and_gives_back_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let disk = Arc::new(Disk::open(vec![dir.path().to_path_buf()], 0).unwrap());
        disk.set_free_for_test(Some(100));

        let first = disk.try_reserve(60).expect("60 of 100 fits");
        assert!(disk.try_reserve(50).is_none(), "only 40 is left");
        drop(first);
        let second = disk.try_reserve(50).expect("the 60 went back");
        assert!(disk.try_reserve(51).is_none());
        drop(second);
    }

    #[test]
    fn free_bytes_of_a_directory_is_positive() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_bytes(dir.path()).unwrap() > 0);
    }
}
