//! Storage-side project state (spec 03 §7 and §8): retention watermarks and
//! dropped projects.
//!
//! Readers and jobs read it from [`crate::read_state::ReadState`].

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;

/// Storage-side project state readers and jobs need: retention watermarks
/// (spec 03 §8) and dropped projects (spec 03 §7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectStates {
    keep_from: HashMap<ProjectId, NaiveDate>,
    /// Both `dropping` and `dropped` projects (spec 03 §7).
    dropped: HashSet<ProjectId>,
}

impl ProjectStates {
    /// The state stored in `catalog.db`, loaded at open.
    pub(crate) fn loaded(
        keep_from: HashMap<ProjectId, NaiveDate>,
        dropped: HashSet<ProjectId>,
    ) -> Self {
        Self { keep_from, dropped }
    }

    /// The project's retention watermark, if any.
    pub(crate) fn keep_from(&self, project: ProjectId) -> Option<NaiveDate> {
        self.keep_from.get(&project).copied()
    }

    /// Whether the project was dropped (spec 03 §7), whatever the state of its
    /// media deletion.
    pub(crate) fn is_dropped(&self, project: ProjectId) -> bool {
        self.dropped.contains(&project)
    }

    /// A copy whose watermark for `project` is `max(current, keep_from)`.
    pub(crate) fn with_keep_from(&self, project: ProjectId, keep_from: NaiveDate) -> Self {
        let mut next = self.clone();
        let watermark = next
            .keep_from(project)
            .map_or(keep_from, |current| current.max(keep_from));
        next.keep_from.insert(project, watermark);
        next
    }

    /// A copy that counts `project` dropped: it gains no watermark back (the
    /// drop commit removed the stored one, spec 03 §7).
    pub(crate) fn with_dropped(&self, project: ProjectId) -> Self {
        let mut next = self.clone();
        next.keep_from.remove(&project);
        next.dropped.insert(project);
        next
    }

    /// Whether rows of `project` on `day` must be dropped.
    pub(crate) fn drops(&self, project: ProjectId, day: NaiveDate) -> bool {
        self.is_dropped(project) || self.keep_from(project).is_some_and(|keep| day < keep)
    }
}
