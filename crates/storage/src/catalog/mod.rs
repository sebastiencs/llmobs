//! In-memory catalog. `catalog.db` is loaded in [`db`] and published by [`committer`].
//!
//! `Storage::open` loads this from `catalog.db` and publishes it with the
//! replayed memtables. A flush replaces it in the same publish that drops
//! `frozen`.
#![allow(dead_code)]

mod committer;
mod db;

pub(crate) use committer::{CatalogEdit, Committer, StagedRemoval, now_ns};

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;
use uuid::Uuid;

use crate::error::CommitError;
use crate::wal::Table;

/// One Parquet object. The inner UUID is the `data_files.id` primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FileId(pub(crate) Uuid);

impl FileId {
    /// A new UUIDv7.
    pub(crate) fn new_v7() -> Self {
        Self(Uuid::now_v7())
    }
}

impl std::fmt::Display for FileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0.as_hyphenated(), formatter)
    }
}

/// Where a catalogued file is allowed to be.
///
/// A remove names the state the row must have. The update then marks it
/// `obsolete` and sets `obsoleted_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileState {
    /// Readable. These rows are the in-memory catalog.
    Active,
    /// Replaced. Kept until M4 collects it.
    Obsolete,
    /// Unreadable. M4 uses this; M2 only accepts it as an expected state.
    Quarantined,
}

impl FileState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Obsolete => "obsolete",
            Self::Quarantined => "quarantined",
        }
    }
}

/// One active file. Immutable once it is inside a published [`Catalog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileMeta {
    /// Primary key.
    pub(crate) id: FileId,
    /// `spans` or `scores`.
    pub(crate) table: Table,
    /// Owning project.
    pub(crate) project: ProjectId,
    /// Partition day.
    pub(crate) day: NaiveDate,
    /// `0` or `1`.
    pub(crate) level: u8,
    /// Path relative to the object-store root.
    pub(crate) path: String,
    /// Object size in bytes.
    pub(crate) size: u64,
    /// Footer size in bytes.
    pub(crate) footer_size: u64,
    /// Row count.
    pub(crate) rows: u64,
    /// Smallest event time, nanoseconds.
    pub(crate) min_time: i64,
    /// Largest event time, nanoseconds.
    pub(crate) max_time: i64,
    /// Smallest ingest time, nanoseconds.
    pub(crate) min_ingest_time: i64,
    /// Largest ingest time, nanoseconds.
    pub(crate) max_ingest_time: i64,
    /// Every row in the file is a merged version.
    pub(crate) fully_merged: bool,
}

/// `spans` then `scores`.
pub(super) type ProjectDays = Arc<BTreeMap<NaiveDate, Vec<Arc<FileMeta>>>>;
pub(super) type Tables = [BTreeMap<ProjectId, ProjectDays>; 2];

/// Active files. Immutable once published: a commit builds a new value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    /// Table, then project, then day. Each day's files are ordered by `(min_time, id)`.
    tables: Tables,
}

impl Catalog {
    /// No files.
    pub(crate) fn empty() -> Self {
        Self {
            tables: [BTreeMap::new(), BTreeMap::new()],
        }
    }

    /// Every active file of `table`, in project then day order.
    #[cfg(test)]
    pub(crate) fn test_files(&self, table: Table) -> Vec<Arc<FileMeta>> {
        self.tables[table_index(table)]
            .values()
            .flat_map(|days| days.values())
            .flatten()
            .map(Arc::clone)
            .collect()
    }

    /// Files for `project` on the inclusive day range, in `(min_time, id)` order.
    ///
    /// A reversed range yields nothing.
    pub(crate) fn files(
        &self,
        table: Table,
        project: ProjectId,
        days: RangeInclusive<NaiveDate>,
    ) -> impl Iterator<Item = &Arc<FileMeta>> {
        let project_days = (days.start() <= days.end())
            .then(|| self.tables[table_index(table)].get(&project))
            .flatten();
        project_days.into_iter().flat_map(move |days_map| {
            days_map
                .range(days.clone())
                .flat_map(|(_, files)| files.iter())
        })
    }

    /// Clone the maps a remove or an add touches. Other projects stay the same `Arc`.
    pub(super) fn apply(&self, edit: &CatalogEdit) -> Result<Self, CommitError> {
        let mut tables = self.tables.clone();
        for (id, state) in &edit.remove {
            if *state != FileState::Active {
                continue;
            }
            if !remove_active(&mut tables, *id) {
                return Err(CommitError::conflict("file is not active in the catalog"));
            }
        }
        for file in &edit.add {
            insert_sorted(&mut tables, file.clone());
        }
        Ok(Self { tables })
    }
}

pub(super) fn table_index(table: Table) -> usize {
    match table {
        Table::Spans => 0,
        Table::Scores => 1,
    }
}

pub(super) fn insert_sorted(tables: &mut Tables, file: FileMeta) {
    let days = tables[table_index(file.table)]
        .entry(file.project)
        .or_insert_with(|| Arc::new(BTreeMap::new()));
    let map = Arc::make_mut(days);
    let files = map.entry(file.day).or_default();
    let key = (file.min_time, file.id);
    let position = files.partition_point(|existing| (existing.min_time, existing.id) < key);
    files.insert(position, Arc::new(file));
}

fn remove_active(tables: &mut Tables, id: FileId) -> bool {
    for table in tables.iter_mut() {
        let Some((project, day)) = find_file(table, id) else {
            continue;
        };
        let Some(days) = table.get_mut(&project) else {
            continue;
        };
        let map = Arc::make_mut(days);
        let Some(files) = map.get_mut(&day) else {
            continue;
        };
        files.retain(|file| file.id != id);
        if files.is_empty() {
            map.remove(&day);
        }
        if map.is_empty() {
            table.remove(&project);
        }
        return true;
    }
    false
}

fn find_file(
    table: &BTreeMap<ProjectId, ProjectDays>,
    id: FileId,
) -> Option<(ProjectId, NaiveDate)> {
    for (project, days) in table {
        for (day, files) in days.iter() {
            if files.iter().any(|file| file.id == id) {
                return Some((*project, *day));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day(value: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 1, value).unwrap()
    }

    fn file(min_time: i64, day: NaiveDate) -> FileMeta {
        FileMeta {
            id: FileId::new_v7(),
            table: Table::Spans,
            project: project(),
            day,
            level: 0,
            path: format!("objects/{min_time}"),
            size: 1,
            footer_size: 1,
            rows: 1,
            min_time,
            max_time: min_time,
            min_ingest_time: min_time,
            max_ingest_time: min_time,
            fully_merged: false,
        }
    }

    #[test]
    fn files_are_ordered_and_a_reversed_range_is_empty() {
        let edit = CatalogEdit {
            add: vec![file(20, day(2)), file(10, day(2)), file(5, day(3))],
            remove: Vec::new(),
            flushed_seq: None,
        };
        let catalog = Catalog::empty().apply(&edit).unwrap();
        let ids: Vec<_> = catalog
            .files(Table::Spans, project(), day(2)..=day(2))
            .map(|file| file.min_time)
            .collect();
        assert_eq!(ids, vec![10, 20]);
        let across: Vec<_> = catalog
            .files(Table::Spans, project(), day(2)..=day(3))
            .map(|file| file.min_time)
            .collect();
        assert_eq!(across, vec![10, 20, 5]);
        assert_eq!(
            catalog
                .files(Table::Spans, project(), day(3)..=day(2))
                .count(),
            0
        );
    }
}
