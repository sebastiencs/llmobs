//! Published memtable snapshots.
//!
//! A query loads one [`ReadState`] and keeps that `Arc` for the whole scan.
//! The writer replaces the `Arc` after each group. M2 adds the catalog here,
//! and M5 adds tombstones.
//!
//! `load` is what a query calls. Open publishes the first state. Both land
//! with `Storage`, so this module is only half-used until then.
#![allow(dead_code)]

use std::sync::{Arc, PoisonError, RwLock};

use crate::memtable::{MemSnapshot, Memtable};
use crate::wal::Table;

/// Snapshots of both tables at one moment.
#[derive(Debug, Clone)]
pub(crate) struct ReadState {
    /// Spans memtable as of this publish.
    pub spans: Arc<MemSnapshot>,
    /// Scores memtable as of this publish.
    pub scores: Arc<MemSnapshot>,
}

impl ReadState {
    /// Both tables empty.
    pub(crate) fn empty() -> Self {
        Self {
            spans: Memtable::new(Table::Spans).snapshot(),
            scores: Memtable::new(Table::Scores).snapshot(),
        }
    }
}

/// The read state readers clone. The writer replaces it.
///
/// A panic while holding the lock poisons it. The stored value is an `Arc`
/// replaced as a whole, so it is never half-written, and `into_inner` keeps
/// the last published state.
#[derive(Debug)]
pub(crate) struct Published(RwLock<Arc<ReadState>>);

impl Published {
    /// Publishes `state`.
    pub(crate) fn new(state: ReadState) -> Self {
        Self(RwLock::new(Arc::new(state)))
    }

    /// Clones the current `Arc`.
    pub(crate) fn load(&self) -> Arc<ReadState> {
        // The stored Arc is replaced as a whole, so a poisoned lock still
        // holds a complete snapshot.
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Builds a new state from the current one and publishes it.
    pub(crate) fn replace(&self, f: impl FnOnce(&ReadState) -> ReadState) {
        // The stored Arc is replaced as a whole, so a poisoned lock still
        // holds a complete snapshot.
        let mut guard = self.0.write().unwrap_or_else(PoisonError::into_inner);
        let next = f(&guard);
        *guard = Arc::new(next);
    }
}
