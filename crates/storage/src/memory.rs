//! Query memory: one process-wide pool shared by every query, and one pool
//! per query over it (spec 03 §6, D-A1-8).
//!
//! A query is admitted by taking its initial bytes from the shared pool. Past
//! those it grows from the shared pool up to its own cap. When the query's
//! runtime is dropped, everything it holds goes back and wakes the queries
//! waiting for admission.

use std::fmt::{self, Display};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use datafusion::error::{DataFusionError, Result as DataFusionResult};
use datafusion::execution::memory_pool::{MemoryLimit, MemoryPool, MemoryReservation};

/// Total memory for the query pool default (spec 03 §6): half of the machine's
/// memory, or of its cgroup limit when that is lower, at most 8 GiB.
///
/// Zero when the OS reports nothing; `Storage::open` then uses `max_memory`.
pub(crate) fn default_total_memory() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut total = system.total_memory();
    if let Some(limits) = system.cgroup_limits()
        && limits.total_memory > 0
    {
        total = total.min(limits.total_memory);
    }
    (total / 2).min(8 << 30)
}

/// Memory shared by every query (spec 03 §6). Counts bytes; it holds none.
#[derive(Debug)]
pub(crate) struct SharedPool {
    limit: usize,
    used: AtomicUsize,
    /// Notified whenever bytes are given back.
    freed: tokio::sync::Notify,
}

impl SharedPool {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            used: AtomicUsize::new(0),
            freed: tokio::sync::Notify::new(),
        }
    }

    /// Takes `bytes` if `used + bytes <= limit`.
    fn try_take(&self, bytes: usize) -> bool {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes).filter(|next| *next <= self.limit) else {
                return false;
            };
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => used = actual,
            }
        }
    }

    /// Takes `bytes` even past the limit: `MemoryPool::grow` cannot fail.
    fn force_take(&self, bytes: usize) {
        self.used.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Gives `bytes` back, saturating at zero, and wakes the waiting admissions.
    fn give_back(&self, bytes: usize) {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let next = used.saturating_sub(bytes);
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(actual) => used = actual,
            }
        }
        self.freed.notify_waiters();
    }

    /// Bytes taken now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    /// Takes `initial` bytes for a new query, waiting until `deadline` for
    /// them. `None` when they never fit.
    pub(crate) async fn admit(
        self: &Arc<Self>,
        initial: usize,
        cap: usize,
        deadline: tokio::time::Instant,
    ) -> Option<Arc<QueryPool>> {
        loop {
            // Register for the wake-up before checking, so a release between
            // the check and the wait is not lost.
            let notified = self.freed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.try_take(initial) {
                return Some(Arc::new(QueryPool::new(Arc::clone(self), initial, cap)));
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return None;
            }
        }
    }
}

/// One query's pool: a [`MemoryPool`] over the shared pool, capped at `cap`.
#[derive(Debug)]
pub(crate) struct QueryPool {
    shared: Arc<SharedPool>,
    /// Taken at admission; never given back before the pool is dropped.
    initial: usize,
    cap: usize,
    state: Mutex<PoolState>,
}

#[derive(Debug, Default)]
struct PoolState {
    /// Bytes the query's reservations hold.
    used: usize,
    /// Bytes taken from the shared pool: `max(initial, used)` at rest.
    held: usize,
}

impl QueryPool {
    /// A pool whose `initial` bytes were just taken from `shared`.
    fn new(shared: Arc<SharedPool>, initial: usize, cap: usize) -> Self {
        Self {
            shared,
            initial,
            cap,
            state: Mutex::new(PoolState {
                used: 0,
                held: initial,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl MemoryPool for QueryPool {
    fn name(&self) -> &str {
        "llmobs_query"
    }

    fn grow(&self, _: &MemoryReservation, additional: usize) {
        let mut guard = self.lock();
        let state = &mut *guard;
        state.used = state.used.saturating_add(additional);
        if state.used > state.held {
            self.shared.force_take(state.used - state.held);
            state.held = state.used;
        }
    }

    fn try_grow(&self, _: &MemoryReservation, additional: usize) -> DataFusionResult<()> {
        let mut guard = self.lock();
        let state = &mut *guard;
        let next = state.used.saturating_add(additional);
        if next > self.cap {
            return Err(DataFusionError::ResourcesExhausted(format!(
                "the query needs more than its {} bytes",
                self.cap
            )));
        }
        if next > state.held && !self.shared.try_take(next - state.held) {
            return Err(DataFusionError::ResourcesExhausted(
                "the shared query memory is in use".to_owned(),
            ));
        }
        state.held = state.held.max(next);
        state.used = next;
        Ok(())
    }

    fn shrink(&self, _: &MemoryReservation, shrink: usize) {
        let mut guard = self.lock();
        let state = &mut *guard;
        state.used = state.used.saturating_sub(shrink);
        let keep = state.used.max(self.initial);
        if state.held > keep {
            self.shared.give_back(state.held - keep);
            state.held = keep;
        }
    }

    fn reserved(&self) -> usize {
        self.lock().used
    }

    fn memory_limit(&self) -> MemoryLimit {
        MemoryLimit::Finite(self.cap)
    }
}

impl Display for QueryPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "llmobs_query(cap={})", self.cap)
    }
}

impl Drop for QueryPool {
    fn drop(&mut self) {
        let held = self
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .held;
        self.shared.give_back(held);
    }
}

#[cfg(test)]
mod tests {
    use datafusion::execution::memory_pool::MemoryConsumer;

    use super::*;

    const MIB: usize = 1024 * 1024;

    /// Admits a pool whose initial bytes are taken now, and registers a
    /// reservation on it. Returns the pool, its `dyn` view and the reservation.
    async fn admitted(
        shared: &Arc<SharedPool>,
        initial: usize,
        cap: usize,
    ) -> (Arc<QueryPool>, Arc<dyn MemoryPool>, MemoryReservation) {
        let pool = shared
            .admit(initial, cap, tokio::time::Instant::now())
            .await
            .unwrap();
        let as_dyn: Arc<dyn MemoryPool> = pool.clone();
        let reservation = MemoryConsumer::new("test").register(&as_dyn);
        (pool, as_dyn, reservation)
    }

    #[tokio::test]
    async fn a_query_pool_fails_past_its_cap() {
        let shared = Arc::new(SharedPool::new(100 * MIB));
        let (_pool, as_dyn, reservation) = admitted(&shared, MIB, 10 * MIB).await;
        reservation.try_grow(9 * MIB).unwrap();
        let error = reservation.try_grow(2 * MIB).unwrap_err();
        assert!(
            matches!(error, DataFusionError::ResourcesExhausted(_)),
            "{error:?}"
        );
        assert_eq!(as_dyn.reserved(), 9 * MIB);
        assert_eq!(shared.used(), 9 * MIB);
    }

    #[tokio::test]
    async fn a_query_pool_fails_when_the_shared_pool_is_full() {
        let shared = Arc::new(SharedPool::new(10 * MIB));
        let (_a, a_dyn, a) = admitted(&shared, MIB, 10 * MIB).await;
        a.try_grow(9 * MIB).unwrap();
        let (_b, _b_dyn, b) = admitted(&shared, MIB, 10 * MIB).await;
        assert_eq!(shared.used(), 10 * MIB);
        assert!(b.try_grow(2 * MIB).is_err());
        b.try_grow(MIB).unwrap();
        assert_eq!(a_dyn.reserved(), 9 * MIB);
    }

    #[tokio::test]
    async fn shrinking_keeps_the_initial_bytes() {
        let shared = Arc::new(SharedPool::new(100 * MIB));
        let (_pool, _as_dyn, reservation) = admitted(&shared, MIB, 10 * MIB).await;
        reservation.try_grow(5 * MIB).unwrap();
        // The query holds 5 MiB in all: its initial MiB is part of them.
        assert_eq!(shared.used(), 5 * MIB);
        reservation.shrink(5 * MIB);
        assert_eq!(shared.used(), MIB);
    }

    #[tokio::test]
    async fn dropping_a_query_pool_gives_everything_back() {
        let shared = Arc::new(SharedPool::new(100 * MIB));
        let (pool, as_dyn, reservation) = admitted(&shared, MIB, 10 * MIB).await;
        reservation.try_grow(5 * MIB).unwrap();
        drop(reservation);
        drop(as_dyn);
        drop(pool);
        assert_eq!(shared.used(), 0);
    }

    // Real time: the workspace's tokio has no `test-util`, so the clock cannot
    // be paused. The waits are short.
    #[tokio::test]
    async fn admission_waits_for_memory_to_come_back() {
        let shared = Arc::new(SharedPool::new(2 * MIB));
        let a = shared
            .admit(2 * MIB, 2 * MIB, tokio::time::Instant::now())
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let waiter = {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move { shared.admit(2 * MIB, 2 * MIB, deadline).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(a);
        assert!(waiter.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn admission_gives_up_at_the_deadline() {
        let shared = Arc::new(SharedPool::new(2 * MIB));
        let _a = shared
            .admit(2 * MIB, 2 * MIB, tokio::time::Instant::now())
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(100);
        assert!(shared.admit(2 * MIB, 2 * MIB, deadline).await.is_none());
    }
}
