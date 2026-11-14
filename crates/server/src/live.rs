//! In-process live bus for SSE (D-G-12, D-G-32, D-G-34).
#![cfg_attr(not(test), allow(dead_code))] // no non-test caller until the streams land (G4.3)

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use llmobs_core::ids::ProjectId;
use llmobs_core::span::Span;
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::api::traces::SpanSummary;

/// One committed batch, or a part of one (at most [`MAX_EVENT_SPANS`] spans).
#[derive(Debug)]
pub(crate) struct LiveEvent {
    /// Monotonic over the process's life, from 1.
    pub(crate) id: u64,
    pub(crate) project_id: ProjectId,
    /// Light rows, in commit order.
    pub(crate) spans: Vec<SpanSummary>,
}

/// Events the channel holds before a slow receiver lags (D-G-32).
pub(crate) const BUS_CAPACITY: usize = 64;
/// Most span summaries in one event (D-G-32).
pub(crate) const MAX_EVENT_SPANS: usize = 500;
/// Open streams one owner may hold (D-G-34).
pub(crate) const MAX_STREAMS_PER_OWNER: u32 = 100;

/// Cadences of the streams (spec: § Timing of the M4 plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamTiming {
    pub(crate) tick: Duration,
    pub(crate) ping: Duration,
    pub(crate) idle: Duration,
}

impl StreamTiming {
    pub(crate) const DEFAULT: Self = Self {
        tick: Duration::from_secs(1),
        ping: Duration::from_secs(15),
        idle: Duration::from_secs(600),
    };

    #[cfg(test)]
    pub(crate) const TEST: Self = Self {
        tick: Duration::from_millis(20),
        ping: Duration::from_millis(200),
        idle: Duration::from_secs(1),
    };
}

/// Who holds a stream slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StreamOwner {
    User(Uuid),
    Key(Uuid),
}

type Slots = Arc<Mutex<HashMap<StreamOwner, u32>>>;

/// The bus every commit publishes to and every stream subscribes to.
pub(crate) struct LiveBus {
    sender: broadcast::Sender<Arc<LiveEvent>>,
    /// The id of the last event sent (0 before the first).
    next_id: AtomicU64,
    slots: Slots,
    closed: watch::Sender<bool>,
    pub(crate) timing: StreamTiming,
}

/// A held stream slot. Dropping it frees the slot.
pub(crate) struct StreamSlot {
    slots: Slots,
    owner: StreamOwner,
}

impl LiveBus {
    pub(crate) fn new(timing: StreamTiming) -> Self {
        Self {
            sender: broadcast::channel(BUS_CAPACITY).0,
            next_id: AtomicU64::new(0),
            slots: Slots::default(),
            closed: watch::channel(false).0,
            timing,
        }
    }

    /// Sends `spans` as events of at most [`MAX_EVENT_SPANS`] summaries. Builds nothing when no
    /// stream is open.
    pub(crate) fn publish(&self, project_id: ProjectId, spans: &[Span]) {
        if self.sender.receiver_count() == 0 {
            return;
        }
        for chunk in spans.chunks(MAX_EVENT_SPANS) {
            let id = self
                .next_id
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            let event = LiveEvent {
                id,
                project_id,
                spans: chunk.iter().map(SpanSummary::from).collect(),
            };
            // An error only means that no receiver is left.
            let _ = self.sender.send(Arc::new(event));
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Arc<LiveEvent>> {
        self.sender.subscribe()
    }

    /// Takes a slot for `owner`, or `None` when the owner holds [`MAX_STREAMS_PER_OWNER`].
    pub(crate) fn acquire(&self, owner: StreamOwner) -> Option<StreamSlot> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let count = slots.entry(owner).or_insert(0);
        if *count >= MAX_STREAMS_PER_OWNER {
            return None;
        }
        *count += 1;
        Some(StreamSlot {
            slots: Arc::clone(&self.slots),
            owner,
        })
    }

    /// Tells every stream to end (shutdown). Receivers taken later see it at once.
    pub(crate) fn close(&self) {
        self.closed.send_replace(true);
    }

    pub(crate) fn closed(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn last_id(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
    }
}

/// Resolves once the bus is closed. Every stream waits on it in its `select!`.
pub(crate) async fn shut(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|closed| *closed).await;
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = slots.get_mut(&self.owner) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                slots.remove(&self.owner);
            }
        }
    }
}

impl fmt::Debug for LiveBus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveBus")
            .field("receivers", &self.sender.receiver_count())
            .field("last_id", &self.next_id.load(Ordering::Relaxed))
            .field("timing", &self.timing)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for StreamSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.owner {
            StreamOwner::User(_) => "user",
            StreamOwner::Key(_) => "key",
        };
        formatter
            .debug_struct("StreamSlot")
            .field("owner", &kind)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestApp, dataset_span};

    const WAIT: Duration = Duration::from_secs(5);

    fn spans(count: usize) -> Vec<Span> {
        (0..count).map(|_| dataset_span(0)).collect()
    }

    #[test]
    fn publish_with_no_subscriber_builds_nothing() {
        let bus = LiveBus::new(StreamTiming::TEST);
        bus.publish(ProjectId::new_v7(), &spans(10));
        assert_eq!(bus.last_id(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_big_commit_is_published_in_events_of_500() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut events = app.state.live.subscribe();
        let batch: Vec<Span> = (0..1200)
            .map(|index| {
                let mut span = dataset_span(0);
                span.name = format!("span-{index}");
                span
            })
            .collect();
        app.commit_spans(&seeded.project, batch).await;

        let mut counts = Vec::new();
        let mut names = Vec::new();
        for expected_id in 1..=3_u64 {
            let event = tokio::time::timeout(WAIT, events.recv())
                .await
                .expect("an event arrives")
                .expect("the bus is open");
            assert_eq!(event.id, expected_id);
            assert_eq!(event.project_id, seeded.project.id);
            counts.push(event.spans.len());
            names.extend(event.spans.iter().map(|span| span.name.clone()));
        }
        assert_eq!(counts, vec![500, 500, 200]);
        let expected: Vec<String> = (0..1200).map(|index| format!("span-{index}")).collect();
        assert_eq!(names, expected);
        assert_eq!(app.state.live.last_id(), 3);
    }

    #[test]
    fn each_owner_holds_one_hundred_slots() {
        let bus = LiveBus::new(StreamTiming::TEST);
        let owner = StreamOwner::User(Uuid::from_u128(1));
        let other = StreamOwner::Key(Uuid::from_u128(2));
        let mut held: Vec<StreamSlot> = (0..MAX_STREAMS_PER_OWNER)
            .map(|_| bus.acquire(owner).expect("a slot is free"))
            .collect();
        assert!(bus.acquire(owner).is_none());
        assert!(bus.acquire(other).is_some());

        held.pop();
        let again = bus.acquire(owner);
        assert!(again.is_some());
        assert!(bus.acquire(owner).is_none());
        drop(again);
        held.clear();
        assert!(bus.slots.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_ends_every_waiter_including_late_ones() {
        let bus = LiveBus::new(StreamTiming::TEST);
        let mut early = bus.closed();
        // The waiter is parked before `close()`, so the close must wake it.
        let waiting = tokio::spawn(async move { shut(&mut early).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.close();
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("a parked waiter is woken by the close")
            .expect("the waiter task does not panic");
        tokio::time::timeout(WAIT, shut(&mut bus.closed()))
            .await
            .expect("a receiver taken after the close returns at once");
    }
}
