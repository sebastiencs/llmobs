//! Pause points and injected faults for storage tests.
//!
//! Each `Storage` owns one [`Hooks`] value, so parallel tests do not share
//! state. There is no process-global hook. Outside tests the methods compile
//! to nothing and the value is zero-sized.

#[cfg(not(test))]
use std::io;

/// Where a test can pause or fail a storage operation.
///
/// Later milestones add points. Existing points keep their names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HookPoint {
    /// `SegmentWriter::write`, before the bytes hit the file.
    WalWrite,
    /// `SegmentWriter::sync`, before `sync_data`.
    WalSync,
    /// After the group is durable, before it is inserted into the memtable.
    AfterWalSync,
    /// After the memtable insert, before the read state is published.
    BeforePublish,
    /// After the read state is published, before the group is acknowledged.
    AfterPublish,
    /// After the catalog writes, before the SQLite commit.
    ///
    /// `fail` rolls the transaction back. The in-memory catalog stays as it was.
    CatalogCommit,
    /// Before the bytes of a local object are written to the temp file.
    ObjectWrite,
    /// Before `sync_all` on a local object temp file.
    ObjectSync,
    /// Before the temp file is renamed to its final name.
    ObjectRename,
    /// Before the directory fsync that publishes a local object.
    ///
    /// The rename has already happened. A fault here moves the file back to its
    /// temp name, so a failed `put_durable` does not leave the final path.
    ObjectDirSync,
    /// After the active memtable is moved to frozen.
    ///
    /// The flush task pauses here after a new freeze, not on a retry.
    FlushAfterFreeze,
    /// After every output path is recorded in `staged_objects`.
    FlushAfterStage,
    /// After every flushed object is durable.
    FlushAfterWrite,
    /// After the catalog commit that publishes the files and drops `frozen`.
    FlushAfterCommit,
    /// After WAL segments covered by `flushed_seq` are deleted and the directory is fsynced.
    FlushAfterTruncate,
    /// Before GC deletes a staged object that is not in the catalog.
    GcBeforeDelete,
    /// Before GC deletes an obsolete file's object.
    GcObsoleteBeforeDelete,
    /// After GC deleted an obsolete file's object, before its row is removed.
    ///
    /// A fault here leaves the row behind; the next collection removes it.
    GcObsoleteAfterDelete,
    /// After a compaction slice's output paths are recorded in `staged_objects`.
    CompactionAfterStage,
    /// After every slice of a compaction job is written and published.
    CompactionAfterWrite,
    /// After the catalog commit that publishes a job's outputs.
    CompactionAfterCommit,
    /// After a media blob's object is durable, before its row is inserted.
    ///
    /// A fault here leaves the object staged and no `media` row: staged-object
    /// GC deletes the object.
    MediaAfterPut,
    /// Before GC deletes an unreferenced media blob's object.
    MediaBeforeDelete,
    /// After the verify pass found a file corrupt, before its quarantine commit.
    ///
    /// `fail` leaves the file active and queues it again.
    QuarantineBeforeCommit,
}

#[cfg(test)]
impl HookPoint {
    const COUNT: usize = 24;

    const fn index(self) -> usize {
        match self {
            Self::WalWrite => 0,
            Self::WalSync => 1,
            Self::AfterWalSync => 2,
            Self::BeforePublish => 3,
            Self::AfterPublish => 4,
            Self::CatalogCommit => 5,
            Self::ObjectWrite => 6,
            Self::ObjectSync => 7,
            Self::ObjectRename => 8,
            Self::ObjectDirSync => 9,
            Self::FlushAfterFreeze => 10,
            Self::FlushAfterStage => 11,
            Self::FlushAfterWrite => 12,
            Self::FlushAfterCommit => 13,
            Self::FlushAfterTruncate => 14,
            Self::GcBeforeDelete => 15,
            Self::GcObsoleteBeforeDelete => 16,
            Self::GcObsoleteAfterDelete => 17,
            Self::CompactionAfterStage => 18,
            Self::CompactionAfterWrite => 19,
            Self::CompactionAfterCommit => 20,
            Self::MediaAfterPut => 21,
            Self::MediaBeforeDelete => 22,
            Self::QuarantineBeforeCommit => 23,
        }
    }
}

/// Per-storage test seam. In production builds every method is a no-op.
#[cfg(not(test))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Hooks;

#[cfg(not(test))]
impl Hooks {
    /// Does nothing outside tests.
    #[inline]
    pub(crate) fn pause(&self, _point: HookPoint) {}

    /// Does nothing outside tests.
    #[inline]
    pub(crate) fn fail(&self, _point: HookPoint) -> Option<io::Error> {
        None
    }

    /// Does nothing outside tests. The writer calls this once per committed group.
    #[inline]
    pub(crate) fn note_group(&self, _requests: usize, _bytes: u64) {}
}

#[cfg(test)]
mod fault {
    use std::collections::HashSet;
    use std::io;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

    use tokio::runtime::RuntimeFlavor;

    use super::HookPoint;

    /// Per-storage test seam. Cloning shares the pause and fault state.
    #[derive(Clone, Debug)]
    pub(crate) struct Hooks {
        inner: Arc<State>,
    }

    struct State {
        held: Mutex<HashSet<HookPoint>>,
        cv: Condvar,
        armed: Mutex<HashSet<HookPoint>>,
        waiting_at: [AtomicUsize; HookPoint::COUNT],
        hits: [AtomicU64; HookPoint::COUNT],
        groups: Mutex<Vec<(usize, u64)>>,
        passed: Mutex<Vec<HookPoint>>,
        abort_at: Mutex<HashSet<HookPoint>>,
    }

    impl std::fmt::Debug for State {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("HookState")
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    impl Default for Hooks {
        fn default() -> Self {
            Self {
                inner: Arc::new(State {
                    held: Mutex::new(HashSet::new()),
                    cv: Condvar::new(),
                    armed: Mutex::new(HashSet::new()),
                    waiting_at: [const { AtomicUsize::new(0) }; HookPoint::COUNT],
                    hits: [const { AtomicU64::new(0) }; HookPoint::COUNT],
                    groups: Mutex::new(Vec::new()),
                    passed: Mutex::new(Vec::new()),
                    abort_at: Mutex::new(HashSet::new()),
                }),
            }
        }
    }

    impl Hooks {
        /// Blocks until [`Self::release`] drops `point`.
        ///
        /// The wait holds the same mutex `release` takes, so a release that
        /// arrives after the point is observed cannot be lost.
        pub(crate) fn pause(&self, point: HookPoint) {
            let index = point.index();
            self.inner.hits[index].fetch_add(1, Ordering::SeqCst);
            lock(&self.inner.passed).push(point);
            let aborting = lock(&self.inner.abort_at).contains(&point);
            if aborting {
                // The crash child is a separate process. Aborting it is the point.
                #[allow(clippy::exit)]
                std::process::abort();
            }
            let mut held = lock(&self.inner.held);
            if !held.contains(&point) {
                return;
            }
            self.inner.waiting_at[index].fetch_add(1, Ordering::SeqCst);
            let wait = || {
                while held.contains(&point) {
                    held = self
                        .inner
                        .cv
                        .wait(held)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            // Async tasks pause too, which blocks a tokio worker. A task the
            // caller just woke sits in that worker's LIFO slot, which other
            // workers never steal; if the test waits on it before releasing,
            // it deadlocks. `block_in_place` hands the slot and the worker's
            // core to another thread. It panics on a current-thread runtime,
            // where the plain wait stays; on blocking-pool threads it just
            // runs the closure.
            let multi_thread = tokio::runtime::Handle::try_current()
                .is_ok_and(|handle| handle.runtime_flavor() == RuntimeFlavor::MultiThread);
            if multi_thread {
                tokio::task::block_in_place(wait);
            } else {
                wait();
            }
            self.inner.waiting_at[index].fetch_sub(1, Ordering::SeqCst);
        }

        /// Takes a one-shot fault armed with [`Self::arm_fail`].
        pub(crate) fn fail(&self, point: HookPoint) -> Option<io::Error> {
            if lock(&self.inner.armed).remove(&point) {
                Some(io::Error::other("injected fault"))
            } else {
                None
            }
        }

        /// Makes the next [`Self::pause`] on `point` wait.
        pub(crate) fn hold(&self, point: HookPoint) {
            lock(&self.inner.held).insert(point);
        }

        /// Unblocks every [`Self::pause`] waiting on `point`.
        pub(crate) fn release(&self, point: HookPoint) {
            let mut held = lock(&self.inner.held);
            held.remove(&point);
            self.inner.cv.notify_all();
        }

        /// The next [`Self::pause`] on `point` aborts the process.
        ///
        /// Used by the flush crash child. The parent reopens the directory.
        pub(crate) fn abort_at(&self, point: HookPoint) {
            lock(&self.inner.abort_at).insert(point);
        }

        /// The next [`Self::fail`] on `point` returns an error, once.
        pub(crate) fn arm_fail(&self, point: HookPoint) {
            lock(&self.inner.armed).insert(point);
        }

        /// How many calls are inside [`Self::pause`] for a held point.
        pub(crate) fn waiting(&self) -> usize {
            self.inner
                .waiting_at
                .iter()
                .map(|count| count.load(Ordering::SeqCst))
                .sum()
        }

        /// How many calls are inside [`Self::pause`] for `point`.
        pub(crate) fn waiting_at(&self, point: HookPoint) -> usize {
            self.inner.waiting_at[point.index()].load(Ordering::SeqCst)
        }

        /// How many times [`Self::pause`] has been entered for `point`.
        ///
        /// This counts every call, including ones that were not held, so a test
        /// can count `WalSync` without pausing the writer.
        pub(crate) fn hits(&self, point: HookPoint) -> u64 {
            self.inner.hits[point.index()].load(Ordering::SeqCst)
        }

        /// Records one committed group. The writer calls this before `AfterWalSync`.
        pub(crate) fn note_group(&self, requests: usize, bytes: u64) {
            lock(&self.inner.groups).push((requests, bytes));
        }

        /// Groups recorded by [`Self::note_group`], in commit order.
        ///
        /// Each entry is `(requests, bytes)`.
        pub(crate) fn groups(&self) -> Vec<(usize, u64)> {
            lock(&self.inner.groups).clone()
        }

        /// Hook points entered through [`Self::pause`], in order.
        ///
        /// M2.9 asserts the durability order from this.
        pub(crate) fn points(&self) -> Vec<HookPoint> {
            lock(&self.inner.passed).clone()
        }
    }
}

#[cfg(test)]
pub(crate) use fault::Hooks;

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{HookPoint, Hooks};

    #[test]
    fn pause_blocks_until_the_point_is_released() {
        let hooks = Hooks::default();
        hooks.hold(HookPoint::AfterWalSync);
        let parked = hooks.clone();
        let thread = thread::spawn(move || parked.pause(HookPoint::AfterWalSync));
        let started = Instant::now();
        while hooks.waiting() == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "pause did not block"
            );
            thread::yield_now();
        }
        hooks.release(HookPoint::AfterWalSync);
        thread.join().unwrap();
        assert_eq!(hooks.waiting(), 0);
    }

    /// A task woken just before a worker parks on a held point sits in that
    /// worker's LIFO slot, which other workers never steal. The pause must hand
    /// the slot to another thread, or a test that waits on the woken task
    /// before releasing the point deadlocks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_task_woken_before_a_held_pause_still_runs() {
        use tokio::sync::oneshot;
        use tokio::time::timeout;

        let hooks = Hooks::default();
        hooks.hold(HookPoint::FlushAfterWrite);

        // The woken task: polled once so it waits on `wake`, then reports.
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let (wake_tx, wake_rx) = oneshot::channel::<()>();
        let (ran_tx, ran_rx) = oneshot::channel::<()>();
        let woken = tokio::spawn(async move {
            ready_tx.send(()).unwrap();
            wake_rx.await.unwrap();
            // The receiver is gone if the timeout already fired.
            let _ = ran_tx.send(());
        });
        ready_rx.await.unwrap();

        // The pausing task wakes it from a worker (LIFO slot), then parks.
        let parked = hooks.clone();
        let pausing = tokio::spawn(async move {
            wake_tx.send(()).unwrap();
            parked.pause(HookPoint::FlushAfterWrite);
        });

        let ran = timeout(Duration::from_secs(5), ran_rx).await;
        // Release before asserting, so a regression fails instead of hanging.
        hooks.release(HookPoint::FlushAfterWrite);
        pausing.await.unwrap();
        woken.await.unwrap();
        assert!(
            ran.is_ok(),
            "the woken task did not run while the pause was held"
        );
    }

    #[test]
    fn a_pause_on_a_current_thread_runtime_waits_in_place() {
        let hooks = Hooks::default();
        hooks.hold(HookPoint::FlushAfterWrite);
        let parked = hooks.clone();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async move { parked.pause(HookPoint::FlushAfterWrite) });
        });
        let started = Instant::now();
        while hooks.waiting() == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "pause did not block"
            );
            thread::yield_now();
        }
        hooks.release(HookPoint::FlushAfterWrite);
        thread.join().unwrap();
    }

    #[test]
    fn an_armed_fault_fires_once() {
        let hooks = Hooks::default();
        assert!(hooks.fail(HookPoint::BeforePublish).is_none());
        hooks.arm_fail(HookPoint::BeforePublish);
        let error = hooks.fail(HookPoint::BeforePublish).expect("armed fault");
        assert_eq!(error.to_string(), "injected fault");
        assert!(hooks.fail(HookPoint::BeforePublish).is_none());
    }
}
