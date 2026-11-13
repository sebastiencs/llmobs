//! Storage-side project state (spec 03 §8): retention watermarks.
//!
//! Readers and jobs read it from [`crate::read_state::ReadState`]; M5.7 adds
//! dropped projects here too.

use std::collections::HashMap;

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;

/// Storage-side project state readers and jobs need: retention watermarks
/// (M5.5) and dropped projects (M5.7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectStates {
    keep_from: HashMap<ProjectId, NaiveDate>,
}

impl ProjectStates {
    /// The watermarks stored in `catalog.db`, loaded at open.
    pub(crate) fn loaded(keep_from: HashMap<ProjectId, NaiveDate>) -> Self {
        Self { keep_from }
    }

    /// The project's retention watermark, if any.
    pub(crate) fn keep_from(&self, project: ProjectId) -> Option<NaiveDate> {
        self.keep_from.get(&project).copied()
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

    /// Whether rows of `project` on `day` must be dropped.
    pub(crate) fn drops(&self, project: ProjectId, day: NaiveDate) -> bool {
        self.keep_from(project).is_some_and(|keep| day < keep)
    }
}
