//! Deletion requests, their tombstone set, and the covered-row rule (spec 03 §7).
//!
//! M5.1 stores and loads them; queries, flush, compaction and the rewriter
//! (M5.2 on) read them.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use llmobs_core::ids::{ProjectId, TraceId};
use uuid::Uuid;

/// One deletion request handed over by the server (spec 03 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    /// The server's `meta.db` id, UUIDv7. Registration is idempotent by it.
    pub id: Uuid,
    /// The project whose data the deletion covers.
    pub project: ProjectId,
    /// Rows with `ingest_time` at or before this (ns) are covered.
    pub cutoff: i64,
    /// Which rows the deletion covers.
    pub scope: DeletionScope,
}

/// Which rows a deletion covers (spec 03 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionScope {
    /// The rows of the listed traces.
    Traces {
        /// Covered trace ids.
        trace_ids: Vec<TraceId>,
    },
    /// The rows of one user: their trace ids, their user id and, on `scores`
    /// only, their session ids.
    User {
        /// The user the deletion is about.
        user_id: String,
        /// Covered trace ids.
        trace_ids: Vec<TraceId>,
        /// Covered session ids; they match `scores` rows only.
        session_ids: Vec<String>,
    },
}

/// How far a deletion has progressed (spec 03 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionStatus {
    /// Registered; its rows are hidden by tombstones.
    Pending,
    /// Every file and memtable batch that could hold a covered row has been
    /// rewritten or checked, at `applied_at` (ns).
    Applied {
        /// When the deletion was marked applied, ns.
        applied_at: i64,
    },
}

/// A pending deletion as the read path and the jobs use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingDeletion {
    pub(crate) id: Uuid,
    pub(crate) seq: u64,
    pub(crate) project: ProjectId,
    pub(crate) cutoff: i64,
    pub(crate) registered_at: i64,
    pub(crate) trace_ids: HashSet<[u8; 16]>,
    /// Empty for a `Traces` scope, one entry for a `User` scope.
    pub(crate) users: HashSet<String>,
    /// Empty for a `Traces` scope.
    pub(crate) sessions: HashSet<String>,
}

/// Every pending deletion, by project, plus the known sequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tombstones {
    known_seq: u64,
    by_project: HashMap<ProjectId, Arc<[Arc<PendingDeletion>]>>,
}

impl Tombstones {
    /// The highest `deletion_seq` registered so far, pending or applied, or 0.
    pub(crate) fn known_seq(&self) -> u64 {
        self.known_seq
    }

    /// The project's pending deletions in `seq` order; empty when it has none.
    pub(crate) fn for_project(&self, project: ProjectId) -> Arc<[Arc<PendingDeletion>]> {
        self.by_project
            .get(&project)
            .map_or_else(|| Arc::from(Vec::new()), Arc::clone)
    }

    /// Every pending deletion, in `seq` order.
    pub(crate) fn all(&self) -> Vec<Arc<PendingDeletion>> {
        let mut all: Vec<_> = self
            .by_project
            .values()
            .flat_map(|deletions| deletions.iter().map(Arc::clone))
            .collect();
        all.sort_by_key(|deletion| deletion.seq);
        all
    }

    /// A copy with `deletion` added and `known_seq = max(known_seq, deletion.seq)`.
    ///
    /// The project's slice stays sorted by `seq`.
    pub(crate) fn with(&self, deletion: PendingDeletion) -> Tombstones {
        let project = deletion.project;
        let seq = deletion.seq;
        let mut by_project = self.by_project.clone();
        let mut list: Vec<Arc<PendingDeletion>> = by_project
            .remove(&project)
            .map(|slice| slice.iter().cloned().collect())
            .unwrap_or_default();
        let position = list.partition_point(|existing| existing.seq <= seq);
        list.insert(position, Arc::new(deletion));
        by_project.insert(project, list.into());
        Tombstones {
            known_seq: self.known_seq.max(seq),
            by_project,
        }
    }

    /// A copy without the deletion `id` of `project`. `known_seq` is unchanged.
    ///
    /// A project whose last deletion goes is removed from the map.
    pub(crate) fn without(&self, project: ProjectId, id: Uuid) -> Tombstones {
        let mut by_project = self.by_project.clone();
        if let Some(slice) = by_project.remove(&project) {
            let mut list: Vec<Arc<PendingDeletion>> = slice.iter().cloned().collect();
            list.retain(|deletion| deletion.id != id);
            if !list.is_empty() {
                by_project.insert(project, list.into());
            }
        }
        Tombstones {
            known_seq: self.known_seq,
            by_project,
        }
    }

    /// The tombstones loaded from `catalog.db`: `deletions` in any order, under
    /// the `known_seq` read beside them (the highest sequence overall, pending
    /// or applied). Each project's slice is sorted by `seq`.
    pub(crate) fn loaded(known_seq: u64, deletions: Vec<PendingDeletion>) -> Tombstones {
        let mut by_project: HashMap<ProjectId, Vec<Arc<PendingDeletion>>> = HashMap::new();
        for deletion in deletions {
            by_project
                .entry(deletion.project)
                .or_default()
                .push(Arc::new(deletion));
        }
        Tombstones {
            known_seq,
            by_project: by_project
                .into_iter()
                .map(|(project, mut list)| {
                    list.sort_by_key(|deletion| deletion.seq);
                    (project, list.into())
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(which: u8) -> ProjectId {
        format!("018f6b4e-7c1a-7c1a-8c1a-0000000000{which:02x}")
            .parse()
            .unwrap()
    }

    fn pending(project: ProjectId, seq: u64) -> PendingDeletion {
        PendingDeletion {
            id: Uuid::from_u128(u128::from(seq)),
            seq,
            project,
            cutoff: 1,
            registered_at: 2,
            trace_ids: HashSet::new(),
            users: HashSet::new(),
            sessions: HashSet::new(),
        }
    }

    #[test]
    fn with_keeps_seq_order() {
        let one = project(1);
        let tombstones = Tombstones::default()
            .with(pending(one, 3))
            .with(pending(one, 1))
            .with(pending(one, 2));
        let seqs: Vec<_> = tombstones
            .for_project(one)
            .iter()
            .map(|deletion| deletion.seq)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(tombstones.known_seq(), 3);
        let all: Vec<_> = tombstones
            .all()
            .iter()
            .map(|deletion| deletion.seq)
            .collect();
        assert_eq!(all, vec![1, 2, 3]);
    }

    #[test]
    fn without_removes_and_drops_an_emptied_project() {
        let one = project(1);
        let two = project(2);
        let first = pending(one, 1);
        let id = first.id;
        let tombstones = Tombstones::default()
            .with(first)
            .with(pending(two, 2))
            .without(one, id);
        assert!(tombstones.for_project(one).is_empty());
        assert!(!tombstones.by_project.contains_key(&one));
        assert_eq!(tombstones.for_project(two).len(), 1);
        assert_eq!(tombstones.known_seq(), 2);
    }

    #[test]
    fn for_project_of_an_unknown_project_is_empty() {
        let tombstones = Tombstones::default().with(pending(project(1), 1));
        assert!(tombstones.for_project(project(2)).is_empty());
    }
}
