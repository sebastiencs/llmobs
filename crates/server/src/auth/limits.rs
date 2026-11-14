//! In-memory rate limits and login lockout (D143).
//!
//! A flood of distinct keys can evict a real user's bucket. That user then
//! starts again with a full bucket, so the flood never refuses someone who
//! still had tokens, and it never locks an email that was not already locked.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

/// How many checks are allowed, refilled evenly over `per`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bucket {
    /// Tokens available after the window has fully refilled.
    pub(crate) capacity: u32,
    /// The window those tokens are spread across.
    pub(crate) per: Duration,
}

/// One check came too soon. The duration is whole seconds, and at least one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryAfter(pub(crate) Duration);

/// A token bucket per key. At most `max_keys` entries are kept.
pub(crate) struct KeyedLimiter<K> {
    bucket: Bucket,
    max_keys: usize,
    /// Remaining tokens and the last refill time, in nanoseconds.
    state: Mutex<HashMap<K, (f64, i64)>>,
}

impl<K: Hash + Eq + Clone> KeyedLimiter<K> {
    pub(crate) fn new(bucket: Bucket, max_keys: usize) -> Self {
        Self {
            bucket,
            max_keys,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token. A missing key starts full and then spends one.
    ///
    /// # Errors
    ///
    /// Returns [`RetryAfter`] when the key has less than one token.
    pub(crate) fn check(&self, key: &K, now: i64) -> Result<(), RetryAfter> {
        let mut state = lock(&self.state);
        if let Some(entry) = state.get_mut(key) {
            return take(entry, &self.bucket, now);
        }
        let bucket = self.bucket;
        make_room(
            &mut state,
            self.max_keys,
            |(tokens, updated)| {
                refilled(*tokens, *updated, now, &bucket) >= f64::from(bucket.capacity)
            },
            |(_, updated)| *updated,
        );
        let tokens = f64::from(self.bucket.capacity) - 1.0;
        state.insert(key.clone(), (tokens, now));
        Ok(())
    }

    /// Drops keys whose buckets have refilled to `capacity`.
    pub(crate) fn prune(&self, now: i64) {
        let mut state = lock(&self.state);
        let bucket = self.bucket;
        state.retain(|_, (tokens, updated)| {
            refilled(*tokens, *updated, now, &bucket) < f64::from(bucket.capacity)
        });
    }
}

/// One email's failure count. `updated_ns` is how a full map finds the oldest row.
struct Failure {
    count: u32,
    locked_until: Option<i64>,
    updated_ns: i64,
}

/// Consecutive password failures for one email.
pub(crate) struct Lockout {
    threshold: u32,
    duration: Duration,
    max_keys: usize,
    state: Mutex<HashMap<String, Failure>>,
}

impl Lockout {
    pub(crate) fn new(threshold: u32, duration: Duration, max_keys: usize) -> Self {
        Self {
            threshold,
            duration,
            max_keys,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// The email is inside a lock window.
    pub(crate) fn is_locked(&self, email: &str, now: i64) -> bool {
        let state = lock(&self.state);
        state
            .get(email)
            .and_then(|failure| failure.locked_until)
            .is_some_and(|until| now < until)
    }

    /// Counts a failed password login. The `threshold`th failure locks until
    /// `now + duration` and resets the count. A failure during a lock does not
    /// extend it.
    pub(crate) fn record_failure(&self, email: &str, now: i64) {
        let mut state = lock(&self.state);
        if let Some(failure) = state.get(email)
            && failure.locked_until.is_some_and(|until| now < until)
        {
            return;
        }
        let count = state
            .get(email)
            .map(|failure| failure.count)
            .unwrap_or(0)
            .saturating_add(1);
        let failure = if count >= self.threshold {
            Failure {
                count: 0,
                locked_until: Some(now.saturating_add(duration_ns(self.duration))),
                updated_ns: now,
            }
        } else {
            Failure {
                count,
                locked_until: None,
                updated_ns: now,
            }
        };
        if !state.contains_key(email) {
            make_room(
                &mut state,
                self.max_keys,
                |failure| failure.locked_until.is_some_and(|until| now >= until),
                |failure| failure.updated_ns,
            );
        }
        state.insert(email.to_owned(), failure);
    }

    /// Forgets failures and any lock for this email.
    pub(crate) fn clear(&self, email: &str) {
        lock(&self.state).remove(email);
    }
}

/// `POST /mcp` requests one token may make per minute (D-H-10).
pub(crate) const MCP_PER_MINUTE: u32 = 60;
/// `POST /mcp` requests one token may have in progress at once (D-H-10).
pub(crate) const MCP_IN_FLIGHT: u32 = 4;

/// Rate and concurrency of `POST /mcp`, per personal access token id.
pub(crate) struct McpLimits {
    rate: KeyedLimiter<Uuid>,
    /// Requests in progress per token. A token with none has no entry.
    in_flight: Arc<Mutex<HashMap<Uuid, u32>>>,
}

impl McpLimits {
    fn new(max_keys: usize) -> Self {
        Self {
            rate: KeyedLimiter::new(
                Bucket {
                    capacity: MCP_PER_MINUTE,
                    per: Duration::from_secs(60),
                },
                max_keys,
            ),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Admits one request of `token`. The request counts as in progress until the guard drops.
    ///
    /// # Errors
    ///
    /// [`RetryAfter`] of 1 s when `token` already has [`MCP_IN_FLIGHT`] requests in progress
    /// (no rate token is spent), or the rate limiter's [`RetryAfter`].
    pub(crate) fn admit(&self, token: Uuid, now: i64) -> Result<McpGuard, RetryAfter> {
        let mut in_flight = lock(&self.in_flight);
        let current = in_flight.get(&token).copied().unwrap_or(0);
        if current >= MCP_IN_FLIGHT {
            return Err(RetryAfter(Duration::from_secs(1)));
        }
        self.rate.check(&token, now)?;
        in_flight.insert(token, current + 1);
        Ok(McpGuard {
            in_flight: Arc::clone(&self.in_flight),
            token,
        })
    }

    fn prune(&self, now: i64) {
        self.rate.prune(now);
    }

    /// Requests of `token` in progress. Tests only.
    #[cfg(test)]
    pub(crate) fn in_flight(&self, token: Uuid) -> u32 {
        lock(&self.in_flight).get(&token).copied().unwrap_or(0)
    }
}

/// One admitted `POST /mcp` request. Dropping it ends the request's in-progress slot.
pub(crate) struct McpGuard {
    in_flight: Arc<Mutex<HashMap<Uuid, u32>>>,
    token: Uuid,
}

impl Drop for McpGuard {
    fn drop(&mut self) {
        let mut in_flight = lock(&self.in_flight);
        if let Some(count) = in_flight.get_mut(&self.token) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.token);
            }
        }
    }
}

/// The login, form, and mail limiters, plus the password lockout.
pub(crate) struct Limits {
    /// 10 checks per minute per `(client IP, email)`.
    pub(crate) login: KeyedLimiter<(IpAddr, String)>,
    /// 5 checks per minute per client IP.
    pub(crate) ip_forms: KeyedLimiter<IpAddr>,
    /// 3 checks per hour per email.
    pub(crate) email_mail: KeyedLimiter<String>,
    /// 10 requests per second per (key id, client IP) on public feedback (D-G-16).
    pub(crate) feedback: KeyedLimiter<(Uuid, IpAddr)>,
    /// 60 requests per minute and 4 at once per personal access token on `POST /mcp` (D-H-10).
    pub(crate) mcp: McpLimits,
    /// 20 failures lock password login for 15 minutes.
    pub(crate) lockout: Lockout,
}

impl Limits {
    /// The D143 limits. Each map keeps at most 100 000 keys.
    pub(crate) fn new() -> Self {
        const MAX_KEYS: usize = 100_000;
        Self {
            login: KeyedLimiter::new(
                Bucket {
                    capacity: 10,
                    per: Duration::from_secs(60),
                },
                MAX_KEYS,
            ),
            ip_forms: KeyedLimiter::new(
                Bucket {
                    capacity: 5,
                    per: Duration::from_secs(60),
                },
                MAX_KEYS,
            ),
            email_mail: KeyedLimiter::new(
                Bucket {
                    capacity: 3,
                    per: Duration::from_secs(60 * 60),
                },
                MAX_KEYS,
            ),
            feedback: KeyedLimiter::new(
                Bucket {
                    capacity: 10,
                    per: Duration::from_secs(1),
                },
                MAX_KEYS,
            ),
            mcp: McpLimits::new(MAX_KEYS),
            lockout: Lockout::new(20, Duration::from_secs(15 * 60), MAX_KEYS),
        }
    }

    /// Drops limiter keys that have fully refilled, and expired locks.
    pub(crate) fn prune(&self, now: i64) {
        self.login.prune(now);
        self.ip_forms.prune(now);
        self.email_mail.prune(now);
        self.feedback.prune(now);
        self.mcp.prune(now);
        self.lockout.prune(now);
    }
}

impl Lockout {
    fn prune(&self, now: i64) {
        lock(&self.state).retain(|_, failure| failure.locked_until.is_none_or(|until| now < until));
    }
}

/// A poisoned mutex still holds plain counters, so the guard is recovered.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn take(entry: &mut (f64, i64), bucket: &Bucket, now: i64) -> Result<(), RetryAfter> {
    let tokens = refilled(entry.0, entry.1, now, bucket);
    if tokens >= 1.0 {
        *entry = (tokens - 1.0, now);
        Ok(())
    } else {
        *entry = (tokens, now);
        Err(retry_after(1.0 - tokens, bucket))
    }
}

fn refilled(tokens: f64, updated: i64, now: i64, bucket: &Bucket) -> f64 {
    let elapsed = now.saturating_sub(updated).max(0);
    let per_ns = bucket.per.as_nanos();
    if per_ns == 0 {
        return f64::from(bucket.capacity);
    }
    let added = (elapsed as f64) * f64::from(bucket.capacity) / (per_ns as f64);
    (tokens + added).min(f64::from(bucket.capacity))
}

fn retry_after(missing: f64, bucket: &Bucket) -> RetryAfter {
    let per_ns = bucket.per.as_nanos();
    let wait_ns = if bucket.capacity == 0 || per_ns == 0 {
        1_u128
    } else {
        let nanos = (missing * (per_ns as f64) / f64::from(bucket.capacity)).ceil();
        if nanos.is_finite() && nanos > 0.0 {
            nanos as u128
        } else {
            1
        }
    };
    let seconds = wait_ns.div_ceil(1_000_000_000).max(1);
    RetryAfter(Duration::from_secs(
        u64::try_from(seconds).unwrap_or(u64::MAX),
    ))
}

fn make_room<K, V>(
    state: &mut HashMap<K, V>,
    max_keys: usize,
    mut expired: impl FnMut(&V) -> bool,
    updated: impl Fn(&V) -> i64,
) where
    K: Hash + Eq + Clone,
{
    if state.len() < max_keys {
        return;
    }
    state.retain(|_, value| !expired(value));
    if state.len() < max_keys {
        return;
    }
    let oldest = state
        .iter()
        .min_by_key(|(_, value)| updated(value))
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest {
        state.remove(&key);
    }
}

fn duration_ns(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;
    use std::time::Duration;

    use super::*;
    use crate::clock::Clock;

    #[test]
    fn ten_per_minute_then_one_more_after_six_seconds() {
        let limiter = KeyedLimiter::new(
            Bucket {
                capacity: 10,
                per: Duration::from_secs(60),
            },
            100,
        );
        let clock = Clock::Manual(Arc::new(AtomicI64::new(0)));
        for _ in 0..10 {
            limiter.check(&"ada", clock.now_ns()).unwrap();
        }
        let retry = limiter.check(&"ada", clock.now_ns()).unwrap_err();
        assert_eq!(retry.0, Duration::from_secs(6));
        limiter.check(&"other", clock.now_ns()).unwrap();
        clock.advance(Duration::from_secs(6));
        limiter.check(&"ada", clock.now_ns()).unwrap();
    }

    #[test]
    fn lockout_lasts_fifteen_minutes_and_clear_resets_the_count() {
        let lockout = Lockout::new(20, Duration::from_secs(15 * 60), 100);
        let start = 1_000;
        for _ in 0..19 {
            lockout.record_failure("ada@example.com", start);
            assert!(!lockout.is_locked("ada@example.com", start));
        }
        lockout.record_failure("ada@example.com", start);
        assert!(lockout.is_locked("ada@example.com", start));
        let until = start + i64::try_from(Duration::from_secs(15 * 60).as_nanos()).unwrap();
        assert!(lockout.is_locked("ada@example.com", until - 1));
        assert!(!lockout.is_locked("ada@example.com", until + 1));

        lockout.clear("ada@example.com");
        assert!(!lockout.is_locked("ada@example.com", start));
        for _ in 0..19 {
            lockout.record_failure("ada@example.com", start);
        }
        assert!(!lockout.is_locked("ada@example.com", start));
    }

    #[test]
    fn a_full_map_drops_the_oldest_key() {
        let limiter = KeyedLimiter::<i32>::new(
            Bucket {
                capacity: 10,
                per: Duration::from_secs(60),
            },
            10,
        );
        for key in 0..11 {
            limiter.check(&key, key as i64).unwrap();
        }
        let state = lock(&limiter.state);
        assert_eq!(state.len(), 10);
        assert!(!state.contains_key(&0));
        assert!(state.contains_key(&10));
    }

    #[test]
    fn prune_drops_only_buckets_that_refilled() {
        let limiter = KeyedLimiter::new(
            Bucket {
                capacity: 1,
                per: Duration::from_secs(1),
            },
            100,
        );
        limiter.check(&"full", 0).unwrap();
        limiter.check(&"partial", 500_000_000).unwrap();
        limiter.prune(1_000_000_000);
        let state = lock(&limiter.state);
        assert!(!state.contains_key(&"full"));
        assert!(state.contains_key(&"partial"));
    }

    #[test]
    fn limits_use_the_d143_windows() {
        let limits = Limits::new();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        limits
            .login
            .check(&(ip, "ada@example.com".to_owned()), 0)
            .unwrap();
        limits.ip_forms.check(&ip, 0).unwrap();
        limits
            .email_mail
            .check(&"ada@example.com".to_owned(), 0)
            .unwrap();
        limits.prune(0);
    }

    #[test]
    fn mcp_admits_four_at_once_and_sixty_a_minute() {
        let limits = McpLimits::new(10);
        let token = Uuid::now_v7();
        let mut guards: Vec<McpGuard> = (0..MCP_IN_FLIGHT)
            .map(|_| limits.admit(token, 0).expect("under the in-flight cap"))
            .collect();
        let fifth = limits.admit(token, 0);
        assert!(
            matches!(fifth.as_ref().err(), Some(RetryAfter(wait)) if *wait == Duration::from_secs(1))
        );
        let other = Uuid::now_v7();
        assert!(limits.admit(other, 0).is_ok());
        guards.pop();
        guards.push(limits.admit(token, 0).expect("a slot is free again"));
        assert_eq!(limits.in_flight(token), MCP_IN_FLIGHT);

        // A fresh limiter: 60 requests in one minute, each finished at once, then a refusal.
        let limits = McpLimits::new(10);
        let token = Uuid::now_v7();
        for _ in 0..MCP_PER_MINUTE {
            assert!(limits.admit(token, 0).is_ok());
        }
        let refused = limits.admit(token, 0);
        assert!(matches!(refused.as_ref().err(), Some(RetryAfter(wait)) if *wait > Duration::ZERO));
        let later = limits.admit(token, 1_000_000_000);
        assert!(later.is_ok());
        drop(later);
        assert_eq!(limits.in_flight(token), 0);
    }
}
