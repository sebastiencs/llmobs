//! Published memtable snapshots and the catalog.
//!
//! A query loads one [`ReadState`] and keeps that `Arc` for the whole scan.
//! Each table snapshot holds the active and frozen batches together, so a
//! reader cannot tell a flush in progress from live rows. The writer replaces
//! the `Arc` after each group. M5 adds tombstones and project states.
//!
//! `load` is what a query calls. Open publishes the first state. Both land
//! with `Storage`, so this module is only half-used until then.
#![allow(dead_code)]

use std::sync::{Arc, PoisonError, RwLock};

use crate::catalog::Catalog;
use crate::deletion::Tombstones;
use crate::memtable::{MemSnapshot, Memtable};
use crate::projects::ProjectStates;
use crate::wal::Table;

/// Snapshots of both tables at one moment.
#[derive(Debug, Clone)]
pub(crate) struct ReadState {
    /// Spans memtable as of this publish, active and frozen batches together.
    pub spans: Arc<MemSnapshot>,
    /// Scores memtable as of this publish, active and frozen batches together.
    pub scores: Arc<MemSnapshot>,
    /// Catalog as of this publish.
    pub catalog: Arc<Catalog>,
    /// Pending deletions as of this publish (spec 03 §7).
    pub tombstones: Arc<Tombstones>,
    /// Project states as of this publish: retention watermarks (spec 03 §8).
    pub projects: Arc<ProjectStates>,
}

impl ReadState {
    /// Both tables empty.
    pub(crate) fn empty() -> Self {
        Self {
            spans: Memtable::new(Table::Spans).snapshot(),
            scores: Memtable::new(Table::Scores).snapshot(),
            catalog: Arc::new(Catalog::empty()),
            tombstones: Arc::new(Tombstones::default()),
            projects: Arc::new(ProjectStates::default()),
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

/// Publishes `catalog` with the current memtable snapshots unchanged.
/// Call it inside the committer's publish closure (the committer mutex is held).
pub(crate) fn publish_catalog(published: &Published, catalog: &Arc<Catalog>) {
    published.replace(|state| ReadState {
        spans: Arc::clone(&state.spans),
        scores: Arc::clone(&state.scores),
        catalog: Arc::clone(catalog),
        tombstones: Arc::clone(&state.tombstones),
        projects: Arc::clone(&state.projects),
    });
}

/// Publishes `tombstones` with everything else unchanged.
/// Call it inside the committer's publish closure (the committer mutex is held).
pub(crate) fn publish_tombstones(published: &Published, tombstones: &Arc<Tombstones>) {
    published.replace(|state| ReadState {
        spans: Arc::clone(&state.spans),
        scores: Arc::clone(&state.scores),
        catalog: Arc::clone(&state.catalog),
        tombstones: Arc::clone(tombstones),
        projects: Arc::clone(&state.projects),
    });
}
