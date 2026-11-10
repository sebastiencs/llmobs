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
    ///
    /// The writer task pauses here once it exists.
    #[allow(dead_code)]
    AfterWalSync,
    /// After the memtable insert, before the read state is published.
    ///
    /// The writer task pauses here once it exists.
    #[allow(dead_code)]
    BeforePublish,
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
}

#[cfg(test)]
mod fault {
    use std::collections::HashSet;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

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
        waiting: AtomicUsize,
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
                    waiting: AtomicUsize::new(0),
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
            let mut held = lock(&self.inner.held);
            if !held.contains(&point) {
                return;
            }
            self.inner.waiting.fetch_add(1, Ordering::SeqCst);
            while held.contains(&point) {
                held = self
                    .inner
                    .cv
                    .wait(held)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            self.inner.waiting.fetch_sub(1, Ordering::SeqCst);
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

        /// The next [`Self::fail`] on `point` returns an error, once.
        pub(crate) fn arm_fail(&self, point: HookPoint) {
            lock(&self.inner.armed).insert(point);
        }

        /// How many calls are inside [`Self::pause`] for a held point.
        pub(crate) fn waiting(&self) -> usize {
            self.inner.waiting.load(Ordering::SeqCst)
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
