//! Nanosecond clock for `meta.db` timestamps.
//!
//! Times are `i64` nanoseconds since the Unix epoch, UTC (spec README).
#![cfg_attr(not(test), allow(dead_code))] // rows are timestamped from P6 on

use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicI64, Ordering};
#[cfg(test)]
use std::time::Duration;

/// Where `created_at` and the other timestamps come from.
#[derive(Clone, Debug)]
pub(crate) enum Clock {
    /// The operating system's clock.
    System,
    /// A test clock. [`Self::advance`] moves it.
    #[cfg(test)]
    Manual(Arc<AtomicI64>),
}

impl Clock {
    /// Nanoseconds since the Unix epoch.
    pub(crate) fn now_ns(&self) -> i64 {
        match self {
            Self::System => system_now_ns(),
            #[cfg(test)]
            Self::Manual(nanos) => nanos.load(Ordering::Relaxed),
        }
    }

    /// Moves a [`Self::Manual`] clock forward.
    #[cfg(test)]
    pub(crate) fn advance(&self, by: Duration) {
        let Self::Manual(nanos) = self else {
            panic!("Clock::advance is for Clock::Manual");
        };
        let step = i64::try_from(by.as_nanos()).unwrap_or(i64::MAX);
        let _ = nanos.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(step))
        });
    }

    /// Sets a [`Self::Manual`] clock. Tests only.
    #[cfg(test)]
    pub(crate) fn set(&self, now_ns: i64) {
        let Self::Manual(nanos) = self else {
            panic!("Clock::set is for Clock::Manual");
        };
        nanos.store(now_ns, Ordering::Relaxed);
    }
}

fn system_now_ns() -> i64 {
    // Supported platforms report a clock after the Unix epoch. `i64::MAX`
    // nanoseconds is year 2262, so the duration fits.
    #[allow(clippy::expect_used)]
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch");
    #[allow(clippy::expect_used)]
    i64::try_from(elapsed.as_nanos()).expect("system time fits in i64 nanoseconds")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;
    use std::time::Duration;

    use super::Clock;

    #[test]
    fn system_clock_is_after_the_epoch() {
        assert!(Clock::System.now_ns() > 0);
    }

    #[test]
    fn manual_clock_advances() {
        let clock = Clock::Manual(Arc::new(AtomicI64::new(10)));
        assert_eq!(clock.now_ns(), 10);
        clock.advance(Duration::from_nanos(5));
        assert_eq!(clock.now_ns(), 15);
    }

    #[test]
    fn manual_clock_is_set() {
        let clock = Clock::Manual(Arc::new(AtomicI64::new(10)));
        clock.set(1_000);
        assert_eq!(clock.now_ns(), 1_000);
        clock.set(7);
        assert_eq!(clock.now_ns(), 7);
    }
}
